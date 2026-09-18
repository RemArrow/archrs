//! `ping`, vendoring the `ping` crate (real ICMP echo request/reply
//! packet construction and matching — not something to hand-roll on
//! top of raw sockets) for the wire protocol, with our own CLI/loop
//! around it.
//!
//! Uses an unprivileged `DGRAM` ICMP socket (the crate's own Linux
//! default), which needs no root/`CAP_NET_RAW` as long as
//! `net.ipv4.ping_group_range` permits it — the same mechanism real
//! `ping` uses to work unprivileged on modern Linux. `ttl` in each
//! reply is `None` on this socket type (the crate's own documented
//! limitation: DGRAM sockets on Linux never see the reply's IP
//! header), so unlike real `ping`, the per-line output has no `ttl=`
//! field. This is a real, visible behavior difference from real ping
//! output, not a bug — the information genuinely isn't available
//! through this socket type without dropping to a raw socket (which
//! would reintroduce the privilege requirement this exists to avoid).
//!
//! Scope: `-c COUNT` (default: run until `-c` packets sent, since an
//! unbounded default is a poor fit for a non-interactive dispatch —
//! see below), `-i INTERVAL` (seconds, default 1), `-W TIMEOUT`
//! (seconds, default 1), one target (hostname or IP, resolved via the
//! standard library's own resolver). Not implemented: IPv6-specific
//! flags, `-f`/`-A` (flood/adaptive), `-s` (payload size).
//!
//! Real `ping`'s own default is to run until interrupted (Ctrl-C) —
//! deliberately not replicated as the default here, since an
//! unbounded loop by default is a bad fit for a CLI meant to be
//! scripted or dispatched programmatically. `-c` is required for that
//! reason, unlike real `ping`, where it's optional.

use std::ffi::OsString;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::Duration;
use std::vec::IntoIter;

struct Args {
    target: Option<String>,
    count: Option<u32>,
    interval: f64,
    timeout: f64,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut args = Args {
        target: None,
        count: None,
        interval: 1.0,
        timeout: 1.0,
    };
    let mut iter = argv.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-c" => {
                let n = iter.next().ok_or("-c requires a count")?;
                args.count = Some(n.parse().map_err(|_| format!("invalid -c value: {n}"))?);
            }
            "-i" => {
                let n = iter.next().ok_or("-i requires an interval")?;
                args.interval = n.parse().map_err(|_| format!("invalid -i value: {n}"))?;
            }
            "-W" => {
                let n = iter.next().ok_or("-W requires a timeout")?;
                args.timeout = n.parse().map_err(|_| format!("invalid -W value: {n}"))?;
            }
            _ if arg.starts_with('-') => return Err(format!("unsupported ping flag: {arg}")),
            _ => args.target = Some(arg.clone()),
        }
    }
    Ok(args)
}

fn resolve(host: &str) -> Result<IpAddr, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }
    (host, 0)
        .to_socket_addrs()
        .map_err(|e| format!("{host}: {e}"))?
        .next()
        .map(|addr: SocketAddr| addr.ip())
        .ok_or_else(|| format!("{host}: could not resolve"))
}

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let parsed = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ping: {e}");
            return 2;
        }
    };
    let Some(target) = parsed.target else {
        eprintln!("ping: no target given");
        return 2;
    };
    let Some(count) = parsed.count else {
        eprintln!("ping: -c COUNT is required (see module docs for why)");
        return 2;
    };
    let addr = match resolve(&target) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("ping: {e}");
            return 2;
        }
    };

    println!("PING {target} ({addr})");
    let mut transmitted = 0u32;
    let mut received = 0u32;
    let mut rtts: Vec<Duration> = Vec::new();

    for seq in 0..count {
        transmitted += 1;
        let result = ping::new(addr)
            .timeout(Duration::from_secs_f64(parsed.timeout))
            .seq_cnt(seq as u16)
            .send();
        match result {
            Ok(reply) => {
                received += 1;
                rtts.push(reply.rtt);
                println!(
                    "{} bytes from {}: icmp_seq={} time={:.2} ms",
                    reply.payload.len(),
                    reply.source,
                    seq,
                    reply.rtt.as_secs_f64() * 1000.0
                );
            }
            Err(e) => println!("ping: seq={seq}: {e}"),
        }
        if seq + 1 < count {
            std::thread::sleep(Duration::from_secs_f64(parsed.interval));
        }
    }

    let loss_pct = if transmitted > 0 {
        100.0 * (transmitted - received) as f64 / transmitted as f64
    } else {
        0.0
    };
    println!("--- {target} ping statistics ---");
    println!("{transmitted} packets transmitted, {received} received, {loss_pct:.0}% packet loss");
    if !rtts.is_empty() {
        let min = rtts.iter().min().unwrap().as_secs_f64() * 1000.0;
        let max = rtts.iter().max().unwrap().as_secs_f64() * 1000.0;
        let avg = rtts.iter().map(|d| d.as_secs_f64() * 1000.0).sum::<f64>() / rtts.len() as f64;
        println!("rtt min/avg/max = {min:.2}/{avg:.2}/{max:.2} ms");
    }

    if received == 0 { 1 } else { 0 }
}
