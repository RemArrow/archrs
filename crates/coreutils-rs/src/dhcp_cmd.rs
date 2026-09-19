//! `dhcpc` — a real, one-shot DHCPv4 client: bring an interface up, run
//! the real DISCOVER/OFFER/REQUEST/ACK exchange (RFC 2131) against
//! whatever's actually listening on the network, and configure the
//! interface (address, default route, `/etc/resolv.conf`) from the real
//! lease. The single biggest lever for this project's own "usable, not
//! just boots" goal (Phase 9) — without this, nothing in the image can
//! reach a network at all beyond a manually-assigned static address.
//!
//! Vendors [`dhcproto`](https://github.com/bluecatengineering/dhcproto)
//! (BlueCat Engineering's DHCPv4/v6 parser/encoder, also the foundation
//! of their own `dora` DHCP server) for the actual wire format —
//! genuinely fiddly to get right by hand (variable-length options,
//! RFC 3396 long-option encoding) and not the interesting part of this
//! tool, matching the project's established "vendor a real crate for
//! the fiddly/security-relevant part, hand-roll the orchestration
//! that's actually specific to this project" pattern (`crypt(3)` for
//! password hashing, `libkmod` for module dependency resolution). The
//! actual client *state machine* (send/wait/retry, and — the genuinely
//! project-specific part — calling straight into `ip_cmd.rs`'s own
//! `set_link_up`/`add_address`/`add_default_route` instead of shelling
//! back out to this same binary's own `ip` dispatch a second time) is
//! this project's own, since no generic crate would have the right
//! shape for that anyway.
//!
//! Scope: one interface, one attempt, IPv4 only, foreground (exits once
//! the lease is applied — no daemonized background renewal). Sets the
//! address (`/`-prefixed from the offered subnet mask), a default route
//! via the offered router, and writes `/etc/resolv.conf` from the
//! offered DNS servers.
//!
//! Not implemented: lease renewal/rebinding (T1/T2 timers — a real,
//! meaningful gap for anything running longer than one lease period,
//! deliberately deferred rather than half-implemented), DHCPv6,
//! multiple interfaces in one invocation, `DECLINE`/`RELEASE`,
//! persisting the lease across a restart, any option beyond
//! subnet mask/router/DNS/lease time/server identifier.
//!
//! # Receiving needs a raw socket, not a plain UDP one
//!
//! A plain `UdpSocket` bound to `0.0.0.0:68` can *send* the broadcast
//! DISCOVER/REQUEST fine, but real DHCP servers commonly reply with a
//! **unicast** packet addressed to the newly offered IP — an address
//! this interface doesn't have yet. The kernel's own IP layer drops
//! such a packet before it ever reaches a regular socket (it isn't
//! addressed to anything locally configured), even though the NIC
//! genuinely received it (confirmed directly:
//! `/sys/class/net/*/statistics/rx_packets` incremented by real reply
//! packets that a plain `UdpSocket::recv` never saw). Found the hard
//! way as a real, reproducible failure ("no reply after 4 attempts"
//! against QEMU's own slirp DHCP server) and confirmed root-caused,
//! not guessed: real `dhclient` (ISC) was installed side by side as an
//! independent sanity check, on the exact same interface, in the exact
//! same environment, and it worked immediately — its own startup log
//! even says why: `Listening on LPF/ens3/...` (Linux Packet Filter — a
//! raw `AF_PACKET` socket, not a UDP one). This is standard, universal
//! DHCP client folklore for exactly this reason, not something
//! specific to this project's own environment.
//!
//! Fixed by receiving on a real `AF_PACKET`/`SOCK_DGRAM` socket bound
//! to the interface instead (`open_raw_receiver` below) — `SOCK_DGRAM`
//! mode has the kernel strip the Ethernet header for us, so what
//! arrives is a raw IP packet; `raw_ip_udp_payload` skips the IP
//! header (variable length, from its own IHL field) and the fixed
//! 8-byte UDP header to reach the actual DHCP payload. Sending still
//! uses the plain `UdpSocket` from `bind_socket` (broadcasting from an
//! unconfigured interface is fine at the IP layer; it's only receiving
//! that needs the raw socket). No safe wrapper in the `nix` crate
//! builds a `sockaddr_ll` with a specific interface index for `bind`,
//! so this drops to real `libc::socket`/`bind`/`recv`/`setsockopt`
//! directly — the same category of "no safe abstraction fits, so a
//! small real `unsafe` FFI block is the honest choice" already used
//! elsewhere in this project (e.g. `main.rs`'s own `libc::signal` call).

use std::ffi::OsString;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;
use std::vec::IntoIter;

use dhcproto::v4::{self, Decodable, Decoder, DhcpOption, Encodable, Encoder, MessageType};

use crate::ip_cmd;

const CLIENT_PORT: u16 = 68;
const SERVER_PORT: u16 = 67;
const RECV_TIMEOUT: Duration = Duration::from_secs(5);
const RETRIES: u32 = 4;
const ETH_P_IP: u16 = 0x0800;

fn bind_socket(dev: &str) -> Result<UdpSocket, String> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, CLIENT_PORT))
        .map_err(|e| format!("binding UDP port {CLIENT_PORT}: {e}"))?;
    socket
        .set_broadcast(true)
        .map_err(|e| format!("enabling SO_BROADCAST: {e}"))?;
    // Restricts replies to the interface we're actually configuring —
    // matters on a multi-interface machine (`lo` always exists
    // alongside anything real), and lets us bind port 68 without an
    // "address already in use" conflict if something else is also
    // running DHCP on a different interface.
    nix::sys::socket::setsockopt(
        &socket,
        nix::sys::socket::sockopt::BindToDevice,
        &dev.into(),
    )
    .map_err(|e| format!("binding to device {dev}: {e}"))?;
    socket
        .set_read_timeout(Some(RECV_TIMEOUT))
        .map_err(|e| format!("setting read timeout: {e}"))?;
    Ok(socket)
}

fn send(socket: &UdpSocket, msg: &v4::Message) -> Result<(), String> {
    let mut buf = Vec::new();
    let mut e = Encoder::new(&mut buf);
    msg.encode(&mut e)
        .map_err(|e| format!("encoding DHCP message: {e}"))?;
    socket
        .send_to(&buf, SocketAddrV4::new(Ipv4Addr::BROADCAST, SERVER_PORT))
        .map_err(|e| format!("sending DHCP message: {e}"))?;
    Ok(())
}

/// Opens a raw `AF_PACKET`/`SOCK_DGRAM` socket bound to `ifindex` — see
/// this module's own top-of-file doc comment for why receiving needs
/// this instead of a plain `UdpSocket`. `SOCK_DGRAM` (as opposed to
/// `SOCK_RAW`) mode has the kernel strip the Ethernet header for us on
/// receive, so callers get a raw IP packet directly.
fn open_raw_receiver(ifindex: i32) -> Result<OwnedFd, String> {
    // SAFETY: standard socket(2)/bind(2)/setsockopt(2) calls with
    // correctly-sized, correctly-initialized argument structs for a
    // real AF_PACKET socket — the same shape as any C program opening
    // one, just written out longhand since no safe Rust wrapper builds
    // an interface-specific `sockaddr_ll` for `bind`.
    unsafe {
        let fd = libc::socket(
            libc::AF_PACKET,
            libc::SOCK_DGRAM,
            i32::from(ETH_P_IP.to_be()),
        );
        if fd < 0 {
            return Err(format!(
                "opening AF_PACKET socket: {}",
                std::io::Error::last_os_error()
            ));
        }
        let owned = OwnedFd::from_raw_fd(fd);

        let mut addr: libc::sockaddr_ll = std::mem::zeroed();
        addr.sll_family = libc::AF_PACKET as u16;
        addr.sll_protocol = ETH_P_IP.to_be();
        addr.sll_ifindex = ifindex;
        let ret = libc::bind(
            owned.as_raw_fd(),
            std::ptr::addr_of!(addr).cast(),
            std::mem::size_of::<libc::sockaddr_ll>() as u32,
        );
        if ret < 0 {
            return Err(format!(
                "binding AF_PACKET socket: {}",
                std::io::Error::last_os_error()
            ));
        }

        let tv = libc::timeval {
            tv_sec: RECV_TIMEOUT.as_secs() as libc::time_t,
            tv_usec: 0,
        };
        let ret = libc::setsockopt(
            owned.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            std::ptr::addr_of!(tv).cast(),
            std::mem::size_of::<libc::timeval>() as u32,
        );
        if ret < 0 {
            return Err(format!(
                "setting AF_PACKET receive timeout: {}",
                std::io::Error::last_os_error()
            ));
        }

        Ok(owned)
    }
}

fn raw_recv(fd: &OwnedFd, buf: &mut [u8]) -> std::io::Result<usize> {
    // SAFETY: `buf` is a valid, correctly-sized slice for the duration
    // of the call.
    let n = unsafe { libc::recv(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// Strips a raw IP packet's own (variable-length, from its IHL field)
/// header and the fixed 8-byte UDP header, returning what's left —
/// i.e. the actual DHCP payload — or `None` if this packet isn't a
/// well-formed IPv4/UDP packet addressed to `CLIENT_PORT` at all (real
/// traffic on a shared broadcast segment includes plenty of other
/// protocols; this quietly skips them rather than erroring).
fn raw_ip_udp_payload(packet: &[u8]) -> Option<&[u8]> {
    if packet.len() < 20 {
        return None;
    }
    let version = packet[0] >> 4;
    let ihl = usize::from(packet[0] & 0x0f) * 4;
    if version != 4 || ihl < 20 || packet.len() < ihl + 8 {
        return None;
    }
    const PROTO_UDP: u8 = 17;
    if packet[9] != PROTO_UDP {
        return None;
    }
    let udp = &packet[ihl..];
    let dst_port = u16::from_be_bytes([udp[2], udp[3]]);
    if dst_port != CLIENT_PORT {
        return None;
    }
    Some(&udp[8..])
}

/// Waits (with retries — real DHCP servers are, in general, on the
/// other end of a broadcast and best-effort UDP, so a real client
/// doesn't get to assume the first attempt lands) for a reply with a
/// matching `xid` and one of the wanted message types.
fn recv_matching(
    raw: &OwnedFd,
    xid: u32,
    resend: impl Fn() -> Result<(), String>,
    wanted: &[MessageType],
) -> Result<v4::Message, String> {
    for attempt in 0..RETRIES {
        if attempt > 0 {
            resend()?;
        }
        let deadline = std::time::Instant::now() + RECV_TIMEOUT;
        while std::time::Instant::now() < deadline {
            let mut buf = [0u8; 1500];
            let n = match raw_recv(raw, &mut buf) {
                Ok(n) => n,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(e) => return Err(format!("receiving DHCP reply: {e}")),
            };
            let Some(payload) = raw_ip_udp_payload(&buf[..n]) else {
                continue;
            };
            let Ok(reply) = v4::Message::decode(&mut Decoder::new(payload)) else {
                continue;
            };
            if reply.xid() != xid {
                continue;
            }
            if let Some(mtype) = reply.opts().msg_type()
                && wanted.contains(&mtype)
            {
                return Ok(reply);
            }
        }
    }
    Err(format!(
        "no reply after {RETRIES} attempts (wanted {wanted:?})"
    ))
}

fn netmask_to_prefix_len(mask: Ipv4Addr) -> u8 {
    u32::from(mask).count_ones() as u8
}

fn write_resolv_conf(servers: &[Ipv4Addr]) {
    let body = servers
        .iter()
        .map(|s| format!("nameserver {s}\n"))
        .collect::<String>();
    if let Err(e) = std::fs::write("/etc/resolv.conf", body) {
        eprintln!("dhcpc: warning: could not write /etc/resolv.conf: {e}");
    }
}

fn run_dhcp(dev: &str) -> Result<(), String> {
    let index = ip_cmd::find_link_index(dev)?;
    let link = ip_cmd::get_links()?
        .into_iter()
        .find(|l| l.header.index == index)
        .ok_or_else(|| format!("device \"{dev}\" disappeared"))?;
    let chaddr = ip_cmd::link_hw_bytes(&link)
        .ok_or_else(|| format!("device \"{dev}\" has no hardware address"))?;

    ip_cmd::set_link_up(index, true)?;
    ip_cmd::wait_for_carrier(index, std::time::Duration::from_secs(5))?;

    let socket = bind_socket(dev)?;
    let raw = open_raw_receiver(index as i32)?;

    let mut discover = v4::Message::new(
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        &chaddr,
    );
    discover.set_flags(v4::Flags::default().set_broadcast());
    discover
        .opts_mut()
        .insert(DhcpOption::MessageType(MessageType::Discover));
    discover
        .opts_mut()
        .insert(DhcpOption::ParameterRequestList(vec![
            v4::OptionCode::SubnetMask,
            v4::OptionCode::Router,
            v4::OptionCode::DomainNameServer,
        ]));
    let xid = discover.xid();

    println!("dhcpc: sending DISCOVER on {dev}");
    send(&socket, &discover)?;
    let offer = recv_matching(
        &raw,
        xid,
        || send(&socket, &discover),
        &[MessageType::Offer],
    )?;

    let offered_addr = offer.yiaddr();
    let server_id = match offer.opts().get(v4::OptionCode::ServerIdentifier) {
        Some(DhcpOption::ServerIdentifier(id)) => *id,
        _ => return Err("OFFER had no server identifier".to_string()),
    };
    println!("dhcpc: got OFFER of {offered_addr} from server {server_id}");

    let mut request = v4::Message::new_with_id(
        xid,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::UNSPECIFIED,
        &chaddr,
    );
    request.set_flags(v4::Flags::default().set_broadcast());
    request
        .opts_mut()
        .insert(DhcpOption::MessageType(MessageType::Request));
    request
        .opts_mut()
        .insert(DhcpOption::RequestedIpAddress(offered_addr));
    request
        .opts_mut()
        .insert(DhcpOption::ServerIdentifier(server_id));

    println!("dhcpc: sending REQUEST for {offered_addr}");
    send(&socket, &request)?;
    let ack = recv_matching(
        &raw,
        xid,
        || send(&socket, &request),
        &[MessageType::Ack, MessageType::Nak],
    )?;
    if ack.opts().msg_type() == Some(MessageType::Nak) {
        return Err(format!("server {server_id} NAK'd our REQUEST"));
    }

    let leased_addr = ack.yiaddr();
    let subnet_mask = match ack.opts().get(v4::OptionCode::SubnetMask) {
        Some(DhcpOption::SubnetMask(m)) => *m,
        _ => Ipv4Addr::new(255, 255, 255, 0), // real DHCP servers always send this; a plain fallback for one that somehow doesn't
    };
    let prefix_len = netmask_to_prefix_len(subnet_mask);
    let router = match ack.opts().get(v4::OptionCode::Router) {
        Some(DhcpOption::Router(routers)) => routers.first().copied(),
        _ => None,
    };
    let dns_servers = match ack.opts().get(v4::OptionCode::DomainNameServer) {
        Some(DhcpOption::DomainNameServer(servers)) => servers.clone(),
        _ => Vec::new(),
    };
    let lease_secs = match ack.opts().get(v4::OptionCode::AddressLeaseTime) {
        Some(DhcpOption::AddressLeaseTime(secs)) => *secs,
        _ => 0,
    };

    println!(
        "dhcpc: ACK: {leased_addr}/{prefix_len}, router {router:?}, dns {dns_servers:?}, lease {lease_secs}s"
    );

    ip_cmd::add_address(index, std::net::IpAddr::V4(leased_addr), prefix_len)?;
    if let Some(gw) = router {
        ip_cmd::add_default_route(gw, index, netlink_packet_route::route::RouteProtocol::Dhcp)?;
    }
    if !dns_servers.is_empty() {
        write_resolv_conf(&dns_servers);
    }

    println!("dhcpc: {dev} configured: {leased_addr}/{prefix_len}");
    Ok(())
}

pub fn run(mut args: IntoIter<OsString>) -> i32 {
    args.next(); // argv[0]: our own utility name
    let Some(dev) = args.next().map(|a| a.to_string_lossy().into_owned()) else {
        eprintln!("dhcpc: usage: dhcpc <interface>");
        return 1;
    };
    match run_dhcp(&dev) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("dhcpc: {e}");
            1
        }
    }
}
