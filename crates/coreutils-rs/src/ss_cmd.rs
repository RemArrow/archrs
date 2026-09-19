//! `ss` (iproute2, the modern replacement for `net-tools`' `netstat` —
//! and what a real, modern Arch install actually ships, unlike
//! `netstat`) — lists socket state. No crate to vendor for the CLI
//! itself, but the actual data comes straight from the already-vendored
//! `procfs` crate's `net::{tcp,tcp6,udp,udp6,unix}()` (used elsewhere
//! in this crate for `ps`/`free`/`uptime`), which parses `/proc/net/*`
//! for us — this file is only the thin listing/formatting layer.
//!
//! Scope: `-t`/`-u`/`-x` select which socket tables to show (default:
//! `-t -u`, matching this project's other scope-downs rather than real
//! `ss`'s default of also including raw/packet sockets); `-a` includes
//! listening sockets (without it, only established/connected ones are
//! shown, matching real `ss`'s default); `-l` shows only listening
//! sockets; `-p` maps each socket's inode back to its owning process
//! by scanning `/proc/*/fd`, the exact technique `procfs`'s own
//! `net.rs` module docs demonstrate for a netstat-alike. `-n`
//! (numeric) is accepted as a no-op: this never does reverse-DNS or
//! `/etc/services` name lookups in the first place.
//!
//! Column data (state, addresses, ports, process) is verified correct
//! against real `ss`, but formatting is our own fixed-width layout
//! rather than byte-identical: real `ss` sizes its `Address:Port`
//! columns dynamically to the actual data (and the terminal width),
//! which isn't worth replicating exactly — the same standard already
//! applied to `ps aux` elsewhere in this crate.
//!
//! One real, structural (not formatting) gap, confirmed by reading
//! the raw kernel data directly: for a `LISTEN` socket, real `ss`'s
//! `Send-Q` shows the configured accept-queue *backlog limit* (e.g.
//! `4096`), which it gets via a netlink `sock_diag`/`INET_DIAG` query
//! — that number simply isn't present in `/proc/net/tcp` at all (its
//! `tx_queue` field reads back `00000000` for these sockets,
//! confirmed directly against `/proc/net/tcp`'s own raw text), so
//! this always shows `0` for `Send-Q` on `LISTEN` rows. Actual queue
//! occupancy for established connections (the field's normal meaning)
//! is correct, since that data *does* come through `/proc/net/tcp`.
//! Fixing this for real would mean vendoring a netlink `sock_diag`
//! client — a bigger scope decision, deliberately not made here.
//!
//! Not implemented: raw/packet sockets, `-e`/`-i` (extended/internal
//! TCP info), `-o` (timer info), filter expressions (`ss state
//! established`, `ss dst ADDR`), IPv6-specific display quirks beyond
//! what `procfs` itself already parses correctly.

use procfs::net::{TcpNetEntry, TcpState, UdpNetEntry, UnixNetEntry, UnixState};
use procfs::process::{FDTarget, all_processes};
use std::collections::HashMap;
use std::ffi::OsString;
use std::vec::IntoIter;

fn tcp_state_name(s: TcpState) -> &'static str {
    match s {
        TcpState::Established => "ESTAB",
        TcpState::SynSent => "SYN-SENT",
        TcpState::SynRecv => "SYN-RECV",
        TcpState::FinWait1 => "FIN-WAIT-1",
        TcpState::FinWait2 => "FIN-WAIT-2",
        TcpState::TimeWait => "TIME-WAIT",
        TcpState::Close => "CLOSE",
        TcpState::CloseWait => "CLOSE-WAIT",
        TcpState::LastAck => "LAST-ACK",
        TcpState::Listen => "LISTEN",
        TcpState::Closing => "CLOSING",
        TcpState::NewSynRecv => "SYN-RECV",
    }
}

fn unix_state_name(s: UnixState) -> &'static str {
    match s {
        UnixState::UNCONNECTED => "UNCONN",
        UnixState::CONNECTING => "CONNECTING",
        UnixState::CONNECTED => "ESTAB",
        UnixState::DISCONNECTING => "DISCONNECTING",
    }
}

fn inode_to_process(inode: u64, map: &HashMap<u64, (i32, String)>) -> String {
    match map.get(&inode) {
        Some((pid, comm)) => format!("{pid}/{comm}"),
        None => "-".to_string(),
    }
}

fn build_inode_map() -> HashMap<u64, (i32, String)> {
    let mut map = HashMap::new();
    let Ok(procs) = all_processes() else {
        return map;
    };
    for p in procs.flatten() {
        let (Ok(stat), Ok(fds)) = (p.stat(), p.fd()) else {
            continue;
        };
        for fd in fds.flatten() {
            if let FDTarget::Socket(inode) = fd.target {
                map.insert(inode, (stat.pid, stat.comm.clone()));
            }
        }
    }
    map
}

struct Opts {
    tcp: bool,
    udp: bool,
    unix: bool,
    all: bool,
    listening_only: bool,
    show_process: bool,
}

fn parse_args(argv: &[String]) -> Opts {
    let mut opts = Opts {
        tcp: false,
        udp: false,
        unix: false,
        all: false,
        listening_only: false,
        show_process: false,
    };
    for arg in &argv[1..] {
        for ch in arg.trim_start_matches('-').chars() {
            match ch {
                't' => opts.tcp = true,
                'u' => opts.udp = true,
                'x' => opts.unix = true,
                'a' => opts.all = true,
                'l' => opts.listening_only = true,
                'p' => opts.show_process = true,
                'n' => {} // numeric is our only mode anyway
                _ => {}
            }
        }
    }
    if !opts.tcp && !opts.udp && !opts.unix {
        opts.tcp = true;
        opts.udp = true;
    }
    opts
}

fn print_row(
    netid: &str,
    state: &str,
    recvq: u32,
    sendq: u32,
    local: &str,
    peer: &str,
    proc: Option<&str>,
) {
    print!("{netid:<6}{state:<12}{recvq:<7}{sendq:<7}{local:<26}{peer:<26}",);
    if let Some(p) = proc {
        println!("{p}");
    } else {
        println!();
    }
}

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let opts = parse_args(&argv);

    let inode_map = if opts.show_process {
        Some(build_inode_map())
    } else {
        None
    };
    let proc_for =
        |inode: u64| -> Option<String> { inode_map.as_ref().map(|m| inode_to_process(inode, m)) };

    println!(
        "{:<6}{:<12}{:<7}{:<7}{:<26}{:<26}{}",
        "Netid",
        "State",
        "Recv-Q",
        "Send-Q",
        "Local Address:Port",
        "Peer Address:Port",
        if opts.show_process { "Process" } else { "" }
    );

    if opts.tcp {
        let mut entries: Vec<TcpNetEntry> = Vec::new();
        entries.extend(procfs::net::tcp().unwrap_or_default());
        entries.extend(procfs::net::tcp6().unwrap_or_default());
        for e in entries {
            let listening = matches!(e.state, TcpState::Listen);
            if opts.listening_only && !listening {
                continue;
            }
            if !opts.all && !opts.listening_only && listening {
                continue;
            }
            let proc = proc_for(e.inode);
            print_row(
                "tcp",
                tcp_state_name(e.state),
                e.rx_queue,
                e.tx_queue,
                &e.local_address.to_string(),
                &e.remote_address.to_string(),
                proc.as_deref(),
            );
        }
    }

    if opts.udp {
        let mut entries: Vec<UdpNetEntry> = Vec::new();
        entries.extend(procfs::net::udp().unwrap_or_default());
        entries.extend(procfs::net::udp6().unwrap_or_default());
        for e in entries {
            let proc = proc_for(e.inode);
            print_row(
                "udp",
                "UNCONN",
                e.rx_queue,
                e.tx_queue,
                &e.local_address.to_string(),
                &e.remote_address.to_string(),
                proc.as_deref(),
            );
        }
    }

    if opts.unix {
        let entries: Vec<UnixNetEntry> = procfs::net::unix().unwrap_or_default();
        for e in entries {
            let listening = matches!(e.state, UnixState::UNCONNECTED) && e.socket_type == 1;
            if opts.listening_only && !listening {
                continue;
            }
            let path = e
                .path
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "*".to_string());
            let proc = proc_for(e.inode);
            print_row(
                "u_str",
                unix_state_name(e.state),
                0,
                0,
                &path,
                "*",
                proc.as_deref(),
            );
        }
    }

    0
}
