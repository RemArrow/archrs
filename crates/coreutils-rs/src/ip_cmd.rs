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
//! and owning interface — real data pulled straight from the kernel's
//! own routing tables via `RTM_GETLINK`/`RTM_GETADDR` dump requests.
//!
//! Verified against real `ip addr`/`ip link` on this dev machine: same
//! interfaces (by index and name), same flags (`UP`/`LOWER_UP`/
//! `BROADCAST`/etc. — confirmed bit-for-bit via `LinkFlags`, the same
//! `IFF_*` kernel constants real `ip` decodes), same MTU, same
//! hardware/IPv4/IPv6 addresses and prefix lengths. Formatting is our
//! own (`<FLAG,FLAG>` list from `bitflags`'s own `Display`, not real
//! `ip`'s exact flag-name spelling/ordering) rather than
//! byte-identical — same standard as `ps aux`/`ss` elsewhere in this
//! crate.
//!
//! Not implemented: any write operation (`ip link set`, `ip addr add`,
//! `ip route add`/`ip route` at all — this phase is visibility only,
//! matching `ss` before it), `ip neigh`/`ip rule`, JSON output,
//! filtering by device name (always lists everything, like `ip a`
//! with no arguments).

use netlink_packet_core::{
    NLM_F_DUMP, NLM_F_REQUEST, NetlinkHeader, NetlinkMessage, NetlinkPayload,
};
use netlink_packet_route::RouteNetlinkMessage;
use netlink_packet_route::address::{AddressAttribute, AddressMessage, AddressScope};
use netlink_packet_route::link::{LinkAttribute, LinkMessage};
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

fn get_links() -> Result<Vec<LinkMessage>, String> {
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

fn link_name(link: &LinkMessage) -> String {
    link.attributes
        .iter()
        .find_map(|a| match a {
            LinkAttribute::IfName(name) => Some(name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| format!("if{}", link.header.index))
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

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let Some(subcommand) = argv.get(1).map(String::as_str) else {
        eprintln!("Usage: ip [ addr | link ] show");
        return 1;
    };
    // Real `ip` accepts abbreviated subcommands (`a`, `addr`,
    // `address`, `l`, `link`) -- matched by prefix the same way.
    let show_addrs = !subcommand.is_empty() && "address".starts_with(subcommand);
    let show_links_only = !subcommand.is_empty() && "link".starts_with(subcommand);
    if !show_addrs && !show_links_only {
        eprintln!("ip: unsupported object '{subcommand}' (only 'addr'/'link' listing implemented)");
        return 1;
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
