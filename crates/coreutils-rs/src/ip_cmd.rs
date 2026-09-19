//! `ip` (iproute2) — real interface/address listing via genuine
//! `rtnetlink` (`NETLINK_ROUTE`) requests, not `/proc` scraping (unlike
//! `ss`): no `/proc` file exposes interface flags, hardware addresses,
//! or address scopes/labels the way netlink does.
//!
//! Vendors the `rust-netlink` project's own crates — `netlink-sys`
//! (plain blocking `AF_NETLINK` socket, used here with none of its
//! optional `tokio`/`mio`/`async-io` features enabled, so this stays
//! fully synchronous, the same constraint that ruled out `dig`'s only
//! real DNS crate), `netlink-packet-core` (the generic netlink message
//! envelope/header), and `netlink-packet-route` (the actual
//! `RTM_GETLINK`/`RTM_GETADDR` message and attribute types — the same
//! crate `rtnetlink` itself is built on, just used directly instead of
//! through its async wrapper). This project hand-rolls the
//! request/dump/parse loop (`netlink_dump` below) instead of vendoring
//! `rtnetlink`, since `rtnetlink` itself only exposes an async API.
//!
//! Scope: `ip link show` / `ip addr show` (also invoked as `ip a`/
//! `ip l`, and as a bare `ip addr`/`ip link`, matching real `ip`'s own
//! abbreviation rules) list every interface with its flags, MTU, and
//! hardware address, and every address with its prefix length, scope,
//! and owning interface. `ip route` lists the IPv4 routing table (its
//! own default family, matching real `ip route`'s default — `ip -6
//! route` isn't implemented). All real data pulled straight from the
//! kernel via `RTM_GETLINK`/`RTM_GETADDR`/`RTM_GETROUTE` dump requests.
//!
//! Verified against real `ip addr`/`ip link`/`ip route` on this dev
//! machine: same interfaces (by index and name), same flags (`UP`/
//! `LOWER_UP`/`BROADCAST`/etc. — confirmed bit-for-bit via
//! `LinkFlags`, the same `IFF_*` kernel constants real `ip` decodes),
//! same MTU, same hardware/IPv4/IPv6 addresses and prefix lengths,
//! and the same routes (default route's gateway/device/protocol,
//! plus the on-link subnet route with its correct `scope link`).
//! Formatting is our own (`<FLAG,FLAG>` list from `bitflags`'s own
//! `Display`, not real `ip`'s exact flag-name spelling/ordering)
//! rather than byte-identical — same standard as `ps aux`/`ss`
//! elsewhere in this crate.
//!
//! Phase 9 added real write operations too, the other half of this same
//! `rtnetlink` machinery: `ip link set DEV up|down` (`RTM_SETLINK`),
//! `ip addr add|del CIDR dev DEV` (`RTM_NEWADDR`/`RTM_DELADDR`), and
//! `ip route add default via GW dev DEV` (`RTM_NEWROUTE`) — the same
//! three real code paths `dhcp_cmd.rs`'s DHCP client calls directly
//! (`set_link_up`/`add_address`/`add_default_route`, `pub(crate)`) to
//! actually configure an interface after a real lease, rather than
//! shelling back out to this binary's own CLI a second time.
//!
//! Not implemented: `ip -6 route`, `ip neigh`/`ip rule`, JSON output,
//! filtering by device name for the read side (always lists
//! everything, like `ip a` with no arguments), `ip route add` to a
//! non-default destination or via a non-gateway nexthop, `ip route
//! del`, `ip addr add`'s optional `broadcast`/`label`/`scope` flags.

use netlink_packet_core::{
    NLM_F_ACK, NLM_F_DUMP, NLM_F_REQUEST, NetlinkHeader, NetlinkMessage, NetlinkPayload,
};
use netlink_packet_route::AddressFamily;
use netlink_packet_route::RouteNetlinkMessage;
use netlink_packet_route::address::{AddressAttribute, AddressMessage, AddressScope};
use netlink_packet_route::link::{LinkAttribute, LinkMessage};
use netlink_packet_route::route::{
    RouteAddress, RouteAttribute, RouteHeader, RouteMessage, RouteScope,
};
use netlink_sys::{Socket, SocketAddr, protocols::NETLINK_ROUTE};
use std::collections::HashMap;
use std::ffi::OsString;
use std::vec::IntoIter;

fn nlmsg_align(len: usize) -> usize {
    (len + 3) & !3
}

/// Sends one `RTM_GET*` dump request and collects every reply until
/// the kernel's `NLMSG_DONE` terminator — the same request/multipart-
/// reply shape every rtnetlink dump uses, regardless of message type.
fn netlink_dump(payload: RouteNetlinkMessage) -> Result<Vec<RouteNetlinkMessage>, String> {
    let mut socket =
        Socket::new(NETLINK_ROUTE).map_err(|e| format!("opening netlink socket: {e}"))?;
    socket
        .bind_auto()
        .map_err(|e| format!("binding netlink socket: {e}"))?;
    let kernel_addr = SocketAddr::new(0, 0);
    socket
        .connect(&kernel_addr)
        .map_err(|e| format!("connecting netlink socket: {e}"))?;

    let mut header = NetlinkHeader::default();
    header.flags = NLM_F_REQUEST | NLM_F_DUMP;
    header.sequence_number = 1;
    let mut request = NetlinkMessage::new(header, NetlinkPayload::InnerMessage(payload));
    request.finalize();
    let mut buf = vec![0u8; request.buffer_len()];
    request.serialize(&mut buf);
    socket
        .send(&buf, 0)
        .map_err(|e| format!("sending netlink request: {e}"))?;

    let mut results = Vec::new();
    'recv: loop {
        let (packet, _) = socket
            .recv_from_full()
            .map_err(|e| format!("receiving netlink reply: {e}"))?;
        let mut offset = 0;
        while offset < packet.len() {
            let msg = NetlinkMessage::<RouteNetlinkMessage>::deserialize(&packet[offset..])
                .map_err(|e| format!("parsing netlink reply: {e}"))?;
            let consumed = nlmsg_align(msg.header.length as usize).max(1);
            match msg.payload {
                NetlinkPayload::Done(_) => break 'recv,
                NetlinkPayload::Error(e) => return Err(format!("netlink error: {e:?}")),
                NetlinkPayload::InnerMessage(inner) => results.push(inner),
                _ => {}
            }
            offset += consumed;
        }
    }
    Ok(results)
}

/// Sends one non-dump `RTM_*` request (`NEW`/`SET`/`DEL`) and waits for
/// the kernel's ACK — the write-side counterpart to `netlink_dump`
/// above, which only handles multipart dump replies. A netlink ACK is
/// itself an error message with error code 0, which is exactly what
/// `NetlinkPayload::Error(e)` carries either way — `e.code` distinguishes
/// a real failure (`Some(errno)`) from a plain ACK (`None`).
fn netlink_request(payload: RouteNetlinkMessage, extra_flags: u16) -> Result<(), String> {
    let mut socket =
        Socket::new(NETLINK_ROUTE).map_err(|e| format!("opening netlink socket: {e}"))?;
    socket
        .bind_auto()
        .map_err(|e| format!("binding netlink socket: {e}"))?;
    let kernel_addr = SocketAddr::new(0, 0);
    socket
        .connect(&kernel_addr)
        .map_err(|e| format!("connecting netlink socket: {e}"))?;

    let mut header = NetlinkHeader::default();
    header.flags = NLM_F_REQUEST | NLM_F_ACK | extra_flags;
    header.sequence_number = 1;
    let mut request = NetlinkMessage::new(header, NetlinkPayload::InnerMessage(payload));
    request.finalize();
    let mut buf = vec![0u8; request.buffer_len()];
    request.serialize(&mut buf);
    socket
        .send(&buf, 0)
        .map_err(|e| format!("sending netlink request: {e}"))?;

    let (packet, _) = socket
        .recv_from_full()
        .map_err(|e| format!("receiving netlink reply: {e}"))?;
    let msg = NetlinkMessage::<RouteNetlinkMessage>::deserialize(&packet)
        .map_err(|e| format!("parsing netlink reply: {e}"))?;
    match msg.payload {
        NetlinkPayload::Error(e) if e.code.is_some() => Err(format!("netlink error: {e:?}")),
        NetlinkPayload::Error(_) => Ok(()), // code None == plain ACK
        other => Err(format!("unexpected netlink reply: {other:?}")),
    }
}

/// Looks up an interface's real `rtnetlink` index by name — every write
/// operation below needs this first, same as real `ip` resolving `dev
/// NAME` to an index before building its own request.
pub(crate) fn find_link_index(name: &str) -> Result<u32, String> {
    get_links()?
        .into_iter()
        .find(|l| link_name(l) == name)
        .map(|l| l.header.index)
        .ok_or_else(|| format!("device \"{name}\" does not exist"))
}

/// `ip link set DEV up|down` — `RTM_SETLINK` with `IFF_UP` in both
/// `flags` (the value to set it to) and `change_mask` (which bit this
/// request is actually allowed to change; every other bit in `flags` is
/// ignored by the kernel without its own bit set in `change_mask`).
pub(crate) fn set_link_up(index: u32, up: bool) -> Result<(), String> {
    use netlink_packet_route::link::LinkFlags;
    let mut msg = LinkMessage::default();
    msg.header.index = index;
    msg.header.flags = if up {
        LinkFlags::Up
    } else {
        LinkFlags::empty()
    };
    msg.header.change_mask = LinkFlags::Up;
    netlink_request(RouteNetlinkMessage::SetLink(msg), 0)
}

/// Polls (up to `timeout`) for a link's real carrier (`IFF_LOWER_UP`) to
/// assert. `set_link_up` only sets the *administrative* state
/// (`IFF_UP`) — real hardware/virtio link negotiation is a genuinely
/// separate, asynchronous step that can lag behind it by a real,
/// non-zero amount, found the hard way in `dhcp_cmd.rs`: sending a real
/// DHCP DISCOVER immediately after `set_link_up` returned worked
/// reliably in isolation, but started reproducibly timing out out once
/// real `systemd`/`udev` was added as this project's own init (Phase
/// 13) — confirmed via `operstate: down` at the moment `dhcpc` had
/// already logged "sending DISCOVER", meaning that specific broadcast
/// went out (if at all) before the link was actually ready, not a
/// `dhcpc`-specific regression so much as a pre-existing race that
/// happened to never lose before. A real DHCP client on real hardware
/// (negotiating with a real switch) needs this exact same wait for
/// exactly the same reason, so this belongs in `ip_cmd.rs` itself, not
/// papered over as a test-only sleep.
pub(crate) fn wait_for_carrier(index: u32, timeout: std::time::Duration) -> Result<(), String> {
    use netlink_packet_route::link::LinkFlags;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let up = get_links()?
            .into_iter()
            .find(|l| l.header.index == index)
            .is_some_and(|l| l.header.flags.contains(LinkFlags::LowerUp));
        if up {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("link carrier never came up within {:?}", timeout));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// `ip addr add CIDR dev DEV` — `RTM_NEWADDR`. `NLM_F_CREATE|NLM_F_EXCL`
/// matches real `ip addr add`'s own semantics: create a new address,
/// fail if that exact address already exists (rather than replacing it,
/// which is what plain `NLM_F_CREATE` alone or `NLM_F_REPLACE` would do).
pub(crate) fn add_address(
    index: u32,
    addr: std::net::IpAddr,
    prefix_len: u8,
) -> Result<(), String> {
    use netlink_packet_core::{NLM_F_CREATE, NLM_F_EXCL};
    let family = if addr.is_ipv4() {
        AddressFamily::Inet
    } else {
        AddressFamily::Inet6
    };
    let mut msg = AddressMessage::default();
    msg.header.family = family;
    msg.header.prefix_len = prefix_len;
    msg.header.index = index;
    msg.attributes.push(AddressAttribute::Local(addr));
    msg.attributes.push(AddressAttribute::Address(addr));
    netlink_request(
        RouteNetlinkMessage::NewAddress(msg),
        NLM_F_CREATE | NLM_F_EXCL,
    )
}

/// `ip addr del CIDR dev DEV` — `RTM_DELADDR`.
pub(crate) fn del_address(
    index: u32,
    addr: std::net::IpAddr,
    prefix_len: u8,
) -> Result<(), String> {
    let family = if addr.is_ipv4() {
        AddressFamily::Inet
    } else {
        AddressFamily::Inet6
    };
    let mut msg = AddressMessage::default();
    msg.header.family = family;
    msg.header.prefix_len = prefix_len;
    msg.header.index = index;
    msg.attributes.push(AddressAttribute::Local(addr));
    netlink_request(RouteNetlinkMessage::DelAddress(msg), 0)
}

/// `ip route add default via GW dev DEV` — `RTM_NEWROUTE`, IPv4 main
/// table, matching what real `ip route add default` writes. `proto` is
/// `dhcp` when a `dhcp_cmd.rs` lease installs this route (matching real
/// `dhcpcd`/`NetworkManager`'s own convention — `ip route` shows exactly
/// this in its own `proto` column) and `static` for the CLI path (a
/// human explicitly running `ip route add` — matching real `ip`'s own
/// default when `proto` isn't given on the command line).
pub(crate) fn add_default_route(
    gateway: std::net::Ipv4Addr,
    index: u32,
    proto: netlink_packet_route::route::RouteProtocol,
) -> Result<(), String> {
    use netlink_packet_core::NLM_F_CREATE;
    use netlink_packet_route::route::{RouteFlags, RouteType};
    let mut msg = RouteMessage::default();
    msg.header.address_family = AddressFamily::Inet;
    msg.header.destination_prefix_length = 0;
    msg.header.table = RouteHeader::RT_TABLE_MAIN;
    msg.header.protocol = proto;
    msg.header.scope = RouteScope::Universe;
    msg.header.kind = RouteType::Unicast;
    msg.header.flags = RouteFlags::empty();
    msg.attributes
        .push(RouteAttribute::Gateway(RouteAddress::Inet(gateway)));
    msg.attributes.push(RouteAttribute::Oif(index));
    netlink_request(RouteNetlinkMessage::NewRoute(msg), NLM_F_CREATE)
}

/// Parses `ADDR/PREFIXLEN` (real `ip`'s own CIDR argument shape for
/// `addr add`/`addr del`) into its two parts.
fn parse_cidr(s: &str) -> Result<(std::net::IpAddr, u8), String> {
    let (addr, len) = s
        .split_once('/')
        .ok_or_else(|| format!("\"{s}\" is not in CIDR (ADDR/PREFIXLEN) form"))?;
    let addr: std::net::IpAddr = addr
        .parse()
        .map_err(|e| format!("\"{addr}\" is not a valid address: {e}"))?;
    let len: u8 = len
        .parse()
        .map_err(|e| format!("\"{len}\" is not a valid prefix length: {e}"))?;
    Ok((addr, len))
}

pub(crate) fn get_links() -> Result<Vec<LinkMessage>, String> {
    let replies = netlink_dump(RouteNetlinkMessage::GetLink(LinkMessage::default()))?;
    Ok(replies
        .into_iter()
        .filter_map(|m| match m {
            RouteNetlinkMessage::NewLink(link) => Some(link),
            _ => None,
        })
        .collect())
}

fn get_addresses() -> Result<Vec<AddressMessage>, String> {
    let replies = netlink_dump(RouteNetlinkMessage::GetAddress(AddressMessage::default()))?;
    Ok(replies
        .into_iter()
        .filter_map(|m| match m {
            RouteNetlinkMessage::NewAddress(addr) => Some(addr),
            _ => None,
        })
        .collect())
}

fn get_routes() -> Result<Vec<RouteMessage>, String> {
    let mut req = RouteMessage::default();
    req.header.address_family = AddressFamily::Inet;
    let replies = netlink_dump(RouteNetlinkMessage::GetRoute(req))?;
    Ok(replies
        .into_iter()
        .filter_map(|m| match m {
            // The kernel dump returns every table (main, local,
            // default, any custom ones) unless filtered; real `ip
            // route` with no arguments only shows the main table, so
            // this matches that default rather than the raw dump.
            RouteNetlinkMessage::NewRoute(route)
                if route.header.table == RouteHeader::RT_TABLE_MAIN =>
            {
                Some(route)
            }
            _ => None,
        })
        .collect())
}

fn route_address(route: &RouteMessage, want: fn(&RouteAttribute) -> bool) -> Option<String> {
    route.attributes.iter().find(|a| want(a)).map(|a| {
        let addr = match a {
            RouteAttribute::Destination(addr)
            | RouteAttribute::Gateway(addr)
            | RouteAttribute::PrefSource(addr) => addr,
            _ => unreachable!(),
        };
        match addr {
            RouteAddress::Inet(ip) => ip.to_string(),
            RouteAddress::Inet6(ip) => ip.to_string(),
            _ => "?".to_string(),
        }
    })
}

/// Real `ip route`'s own protocol-name table for the common, everyday
/// values (`kernel`/`boot`/`static`/`dhcp`); anything else falls back
/// to a lowercased `Debug` rendering rather than a numeric code.
fn protocol_name(route: &RouteMessage) -> String {
    use netlink_packet_route::route::RouteProtocol;
    match route.header.protocol {
        RouteProtocol::Kernel => "kernel".to_string(),
        RouteProtocol::Boot => "boot".to_string(),
        RouteProtocol::Static => "static".to_string(),
        RouteProtocol::Dhcp => "dhcp".to_string(),
        RouteProtocol::Ra => "ra".to_string(),
        other => format!("{other:?}").to_lowercase(),
    }
}

fn print_route_line(route: &RouteMessage, links: &HashMap<u32, &LinkMessage>) {
    let dest = route_address(route, |a| matches!(a, RouteAttribute::Destination(_)));
    let mut line = match dest {
        Some(d) => format!("{d}/{}", route.header.destination_prefix_length),
        None => "default".to_string(),
    };
    if let Some(gw) = route_address(route, |a| matches!(a, RouteAttribute::Gateway(_))) {
        line.push_str(&format!(" via {gw}"));
    }
    let oif = route.attributes.iter().find_map(|a| match a {
        RouteAttribute::Oif(idx) => Some(*idx),
        _ => None,
    });
    if let Some(link) = oif.and_then(|idx| links.get(&idx)) {
        line.push_str(&format!(" dev {}", link_name(link)));
    }
    line.push_str(&format!(" proto {}", protocol_name(route)));
    if !matches!(route.header.scope, RouteScope::Universe) {
        line.push_str(&format!(" scope {}", route.header.scope));
    }
    if let Some(src) = route_address(route, |a| matches!(a, RouteAttribute::PrefSource(_))) {
        line.push_str(&format!(" src {src}"));
    }
    if let Some(metric) = route.attributes.iter().find_map(|a| match a {
        RouteAttribute::Priority(p) => Some(*p),
        _ => None,
    }) {
        line.push_str(&format!(" metric {metric}"));
    }
    println!("{line}");
}

fn link_name(link: &LinkMessage) -> String {
    link.attributes
        .iter()
        .find_map(|a| match a {
            LinkAttribute::IfName(name) => Some(name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| format!("if{}", link.header.index))
}

/// Raw hardware-address bytes — what `dhcp_cmd.rs` needs for a DHCP
/// message's `chaddr` field, as opposed to `link_hw_address`'s own
/// colon-hex display string below.
pub(crate) fn link_hw_bytes(link: &LinkMessage) -> Option<Vec<u8>> {
    link.attributes.iter().find_map(|a| match a {
        LinkAttribute::Address(bytes) if !bytes.is_empty() => Some(bytes.clone()),
        _ => None,
    })
}

fn link_hw_address(link: &LinkMessage) -> Option<String> {
    link.attributes.iter().find_map(|a| match a {
        LinkAttribute::Address(bytes) if !bytes.is_empty() => Some(
            bytes
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(":"),
        ),
        _ => None,
    })
}

fn link_mtu(link: &LinkMessage) -> Option<u32> {
    link.attributes.iter().find_map(|a| match a {
        LinkAttribute::Mtu(mtu) => Some(*mtu),
        _ => None,
    })
}

/// Real `ip` renames the raw `RT_SCOPE_*` kernel values for display
/// (most notably `RT_SCOPE_UNIVERSE` → `global`) rather than printing
/// the kernel's own name — this is that same fixed table.
fn scope_name(scope: &AddressScope) -> String {
    match scope {
        AddressScope::Universe => "global".to_string(),
        AddressScope::Site => "site".to_string(),
        AddressScope::Link => "link".to_string(),
        AddressScope::Host => "host".to_string(),
        AddressScope::Nowhere => "nowhere".to_string(),
        AddressScope::Other(n) => n.to_string(),
        _ => "unknown".to_string(),
    }
}

fn addr_ip(addr: &AddressMessage) -> Option<std::net::IpAddr> {
    addr.attributes.iter().find_map(|a| match a {
        AddressAttribute::Local(ip) => Some(*ip),
        AddressAttribute::Address(ip) => Some(*ip),
        _ => None,
    })
}

fn print_link_line(link: &LinkMessage) {
    let flags = format!("{}", link.header.flags);
    let mtu = link_mtu(link).map(|m| m.to_string()).unwrap_or_default();
    println!(
        "{}: {}: <{}> mtu {}",
        link.header.index,
        link_name(link),
        flags.to_uppercase(),
        mtu
    );
    if let Some(hw) = link_hw_address(link) {
        println!("    link/ether {hw}");
    } else {
        println!("    link/none");
    }
}

fn print_addr_lines(index: u32, addrs: &[AddressMessage]) {
    for addr in addrs.iter().filter(|a| a.header.index == index) {
        let Some(ip) = addr_ip(addr) else { continue };
        let family = if ip.is_ipv4() { "inet" } else { "inet6" };
        println!(
            "    {family} {}/{} scope {}",
            ip,
            addr.header.prefix_len,
            scope_name(&addr.header.scope)
        );
    }
}

/// Finds the value following a keyword token (`dev`, `via`) anywhere in
/// `argv` — real `ip`'s own argument order is fairly permissive about
/// where these go, and this project's own callers (the CLI here,
/// `dhcp_cmd.rs`) only ever need the common orderings, not the full
/// grammar.
fn arg_after<'a>(argv: &'a [String], keyword: &str) -> Option<&'a str> {
    argv.iter()
        .position(|a| a == keyword)
        .and_then(|i| argv.get(i + 1))
        .map(String::as_str)
}

fn run_write(
    show_addrs: bool,
    show_links_only: bool,
    show_routes: bool,
    verb: &str,
    argv: &[String],
) -> Result<(), String> {
    if show_links_only && verb == "set" {
        // `ip link set [dev] DEV up|down` — the device name is
        // whatever positional argument isn't "set"/"dev"/"up"/"down".
        let dev = argv
            .iter()
            .skip(3)
            .find(|a| !matches!(a.as_str(), "dev" | "up" | "down"))
            .ok_or("usage: ip link set [dev] DEV up|down")?;
        let up = argv.iter().any(|a| a == "up");
        let down = argv.iter().any(|a| a == "down");
        if up == down {
            return Err("usage: ip link set [dev] DEV up|down".to_string());
        }
        let index = find_link_index(dev)?;
        return set_link_up(index, up);
    }

    if show_addrs && (verb == "add" || verb == "del") {
        let cidr = argv.get(3).ok_or("usage: ip addr add|del CIDR dev DEV")?;
        let dev = arg_after(argv, "dev").ok_or("usage: ip addr add|del CIDR dev DEV")?;
        let (addr, prefix_len) = parse_cidr(cidr)?;
        let index = find_link_index(dev)?;
        return if verb == "add" {
            add_address(index, addr, prefix_len)
        } else {
            del_address(index, addr, prefix_len)
        };
    }

    if show_routes && verb == "add" {
        let is_default = argv.get(3).map(String::as_str) == Some("default");
        if !is_default {
            return Err("only 'ip route add default via GW dev DEV' is implemented".to_string());
        }
        let gw: std::net::Ipv4Addr = arg_after(argv, "via")
            .ok_or("usage: ip route add default via GW dev DEV")?
            .parse()
            .map_err(|e| format!("invalid gateway address: {e}"))?;
        let dev = arg_after(argv, "dev").ok_or("usage: ip route add default via GW dev DEV")?;
        let index = find_link_index(dev)?;
        return add_default_route(
            gw,
            index,
            netlink_packet_route::route::RouteProtocol::Static,
        );
    }

    Err(format!("unsupported operation '{verb}' for this object"))
}

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let Some(subcommand) = argv.get(1).map(String::as_str) else {
        eprintln!("Usage: ip [ addr | link ] show");
        return 1;
    };
    // Real `ip` accepts abbreviated subcommands (`a`, `addr`,
    // `address`, `l`, `link`, `r`, `route`) -- matched by prefix the
    // same way.
    let show_addrs = !subcommand.is_empty() && "address".starts_with(subcommand);
    let show_links_only = !subcommand.is_empty() && "link".starts_with(subcommand);
    let show_routes = !subcommand.is_empty() && "route".starts_with(subcommand);
    if !show_addrs && !show_links_only && !show_routes {
        eprintln!(
            "ip: unsupported object '{subcommand}' (only 'addr'/'link'/'route' listing implemented)"
        );
        return 1;
    }

    let verb = argv.get(2).map(String::as_str).unwrap_or("show");
    if verb != "show" {
        return match run_write(show_addrs, show_links_only, show_routes, verb, &argv) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("ip: {e}");
                1
            }
        };
    }

    if show_routes {
        let links = match get_links() {
            Ok(l) => l,
            Err(e) => {
                eprintln!("ip: {e}");
                return 1;
            }
        };
        let by_index: HashMap<u32, &LinkMessage> =
            links.iter().map(|l| (l.header.index, l)).collect();
        let routes = match get_routes() {
            Ok(r) => r,
            Err(e) => {
                eprintln!("ip: {e}");
                return 1;
            }
        };
        for route in &routes {
            print_route_line(route, &by_index);
        }
        return 0;
    }

    let links = match get_links() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("ip: {e}");
            return 1;
        }
    };

    let addrs: Vec<AddressMessage> = if show_links_only {
        Vec::new()
    } else {
        match get_addresses() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("ip: {e}");
                return 1;
            }
        }
    };

    let mut by_index: HashMap<u32, &LinkMessage> = HashMap::new();
    for link in &links {
        by_index.insert(link.header.index, link);
    }
    let mut indices: Vec<u32> = by_index.keys().copied().collect();
    indices.sort_unstable();

    for index in indices {
        let link = by_index[&index];
        print_link_line(link);
        if show_addrs {
            print_addr_lines(index, &addrs);
        }
    }

    0
}
