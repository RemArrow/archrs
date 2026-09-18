//! `ps`, `free`, and `uptime` — vendoring `procfs` (a real `/proc`
//! parser, already used transitively elsewhere in this dependency
//! tree) for reading process/memory/uptime data, and `users` for
//! uid-to-username lookup, rather than hand-parsing `/proc` ourselves.
//! Real `ps`/`free`/`uptime` come from the separate `procps-ng`
//! project, not `uutils/coreutils`, so there's no `uu_*` crate for any
//! of them.
//!
//! Scope:
//! - `ps`: no args lists processes with a controlling TTY (roughly
//!   POSIX default); `aux`/`-ef`/`-e` lists every process system-wide
//!   with USER/PID/%MEM/VSZ/RSS/TTY/STAT/TIME/CMD columns. `%CPU` is
//!   not implemented (real `ps` computes it from process/system CPU-
//!   time deltas, which needs either two samples over time or reading
//!   system boot time precisely — more than this scope justifies) and
//!   is always printed as `0.0`; documented rather than silently
//!   wrong. Column widths/alignment are our own, not byte-identical to
//!   real `ps` (procps-ng's own formatting has version-specific
//!   quirks) — verified by data correctness (same PIDs/commands
//!   present) instead of an exact diff.
//! - `free`: `-h`/`-m`/`-g`/`-k` (default) unit selection, Mem/Swap
//!   rows. `used` is `total - available`, matching modern `free`
//!   exactly (two earlier, more "obvious" formulas — a plain
//!   `total - free - buffers - cached`, then one additionally
//!   subtracting reclaimable slab — were each tried and each caught
//!   wrong by diffing against the real binary directly; see
//!   ROADMAP.md for the numbers).
//! - `uptime`: elapsed time and 1/5/15-minute load averages. Current
//!   time and logged-in user count (real `uptime`'s leading `up`
//!   line) are not implemented.

use std::ffi::OsString;
use std::vec::IntoIter;

use procfs::{Current, LoadAverage, Meminfo, Uptime};

fn state_char(stat: &procfs::process::Stat) -> char {
    stat.state
}

fn format_time(utime: u64, stime: u64, ticks_per_sec: u64) -> String {
    let total_secs = (utime + stime) / ticks_per_sec.max(1);
    format!("{:02}:{:02}", total_secs / 60, total_secs % 60)
}

fn run_ps_default() {
    println!("{:>7} {:<8} {:<8} CMD", "PID", "TTY", "STAT");
    let Ok(processes) = procfs::process::all_processes() else {
        eprintln!("ps: could not read /proc");
        return;
    };
    for proc in processes.flatten() {
        let Ok(stat) = proc.stat() else { continue };
        if stat.tty_nr == 0 {
            continue; // no controlling terminal: excluded from the default listing
        }
        let cmd = if stat.comm.is_empty() {
            "?".to_string()
        } else {
            stat.comm.clone()
        };
        println!(
            "{:>7} {:<8} {:<8} {}",
            stat.pid,
            "?",
            state_char(&stat),
            cmd
        );
    }
}

fn run_ps_all() {
    let ticks_per_sec = 100u64; // USER_HZ is 100 on every Linux platform this targets
    println!(
        "{:<8} {:>7} {:>5} {:>8} {:>8} {:<8} {:<5} {:>8} COMMAND",
        "USER", "PID", "%MEM", "VSZ", "RSS", "TTY", "STAT", "TIME"
    );
    let Ok(processes) = procfs::process::all_processes() else {
        eprintln!("ps: could not read /proc");
        return;
    };
    let mem_total = Meminfo::current().map(|m| m.mem_total).unwrap_or(1).max(1);

    for proc in processes.flatten() {
        let Ok(stat) = proc.stat() else { continue };
        let uid = proc.uid().unwrap_or(u32::MAX);
        let user = users::get_user_by_uid(uid)
            .map(|u| u.name().to_string_lossy().into_owned())
            .unwrap_or_else(|| uid.to_string());
        let rss_bytes = stat.rss * procfs::page_size();
        let mem_pct = 100.0 * rss_bytes as f64 / mem_total as f64;
        let cmdline = proc
            .cmdline()
            .ok()
            .filter(|c| !c.is_empty())
            .map(|c| c.join(" "))
            .unwrap_or_else(|| format!("[{}]", stat.comm));

        println!(
            "{:<8} {:>7} {:>5.1} {:>8} {:>8} {:<8} {:<5} {:>8} {}",
            user,
            stat.pid,
            mem_pct,
            stat.vsize / 1024,
            rss_bytes / 1024,
            "?",
            state_char(&stat),
            format_time(stat.utime, stat.stime, ticks_per_sec),
            cmdline
        );
    }
}

pub fn run_ps(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let show_all = argv
        .iter()
        .skip(1)
        .any(|a| matches!(a.as_str(), "aux" | "-ef" | "-e" | "-A" | "--all"));
    if show_all {
        run_ps_all();
    } else {
        run_ps_default();
    }
    0
}

#[derive(Clone, Copy)]
enum Unit {
    Kib,
    Mib,
    Gib,
    Human,
}

fn format_amount(kib: u64, unit: Unit) -> String {
    match unit {
        Unit::Kib => kib.to_string(),
        Unit::Mib => (kib / 1024).to_string(),
        Unit::Gib => (kib / (1024 * 1024)).to_string(),
        Unit::Human => {
            let bytes = kib as f64 * 1024.0;
            if bytes >= 1024.0 * 1024.0 * 1024.0 {
                format!("{:.1}Gi", bytes / (1024.0 * 1024.0 * 1024.0))
            } else if bytes >= 1024.0 * 1024.0 {
                format!("{:.1}Mi", bytes / (1024.0 * 1024.0))
            } else {
                format!("{:.1}Ki", bytes / 1024.0)
            }
        }
    }
}

pub fn run_free(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let mut unit = Unit::Kib;
    for arg in argv.iter().skip(1) {
        match arg.as_str() {
            "-h" | "--human" => unit = Unit::Human,
            "-m" | "--mega" => unit = Unit::Mib,
            "-g" | "--giga" => unit = Unit::Gib,
            "-k" | "--kilo" => unit = Unit::Kib,
            _ => {}
        }
    }

    let Ok(mem) = Meminfo::current() else {
        eprintln!("free: could not read /proc/meminfo");
        return 1;
    };

    let total = mem.mem_total / 1024;
    let free = mem.mem_free / 1024;
    let buffers = mem.buffers / 1024;
    let cached = mem.cached / 1024;
    // Reclaimable slab counts as "cache" for buff/cache's purposes.
    let reclaimable = mem.s_reclaimable.unwrap_or(0) / 1024;
    let available = mem
        .mem_available
        .map(|v| v / 1024)
        .unwrap_or(free + buffers + cached);
    // "used" is `total - available`, not a subtraction of the other
    // columns (`total - free - buffers - cached - reclaimable`, which
    // is what "used" traditionally meant, gave a real, confirmed-wrong
    // answer here — modern free redefines "used" this way specifically
    // so it means "genuinely unavailable to new applications" rather
    // than something derived from the buffers/cache breakdown; caught
    // by diffing against the real binary directly, see ROADMAP.md).
    let used = total.saturating_sub(available);
    let shared = mem.shmem.unwrap_or(0) / 1024;
    let buff_cache = buffers + cached + reclaimable;

    let swap_total = mem.swap_total / 1024;
    let swap_free = mem.swap_free / 1024;
    let swap_used = swap_total.saturating_sub(swap_free);

    // Field widths (label 7, then 13/12/12/12/12/12) match real free's
    // own layout exactly, confirmed by measuring its actual output
    // column-by-column rather than guessing.
    println!(
        "{:<7}{:>13}{:>12}{:>12}{:>12}{:>12}{:>12}",
        "", "total", "used", "free", "shared", "buff/cache", "available"
    );
    println!(
        "{:<7}{:>13}{:>12}{:>12}{:>12}{:>12}{:>12}",
        "Mem:",
        format_amount(total, unit),
        format_amount(used, unit),
        format_amount(free, unit),
        format_amount(shared, unit),
        format_amount(buff_cache, unit),
        format_amount(available, unit)
    );
    println!(
        "{:<7}{:>13}{:>12}{:>12}",
        "Swap:",
        format_amount(swap_total, unit),
        format_amount(swap_used, unit),
        format_amount(swap_free, unit)
    );
    0
}

pub fn run_uptime(_args: IntoIter<OsString>) -> i32 {
    let Ok(uptime) = Uptime::current() else {
        eprintln!("uptime: could not read /proc/uptime");
        return 1;
    };
    let total_secs = uptime.uptime as u64;
    let days = total_secs / 86400;
    let hours = (total_secs % 86400) / 3600;
    let minutes = (total_secs % 3600) / 60;

    let uptime_str = if days > 0 {
        format!("{days} days, {hours:02}:{minutes:02}")
    } else {
        format!("{hours:02}:{minutes:02}")
    };

    match LoadAverage::current() {
        Ok(load) => {
            println!(
                "up {uptime_str}, load average: {:.2}, {:.2}, {:.2}",
                load.one, load.five, load.fifteen
            );
        }
        Err(_) => println!("up {uptime_str}"),
    }
    0
}
