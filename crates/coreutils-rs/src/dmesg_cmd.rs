//! `dmesg` (util-linux, not GNU/uutils) — prints the kernel ring buffer.
//! No crate exists to vendor here: the actual "engine" is the kernel's
//! own `/dev/kmsg` device, a structured, record-oriented interface
//! documented in `Documentation/ABI/testing/dev-kmsg` — each `read()`
//! returns exactly one record (`<facility*8+level>,seq,timestamp_usec,
//! flags[,extra];message`, optionally followed by ` KEY=value`
//! continuation lines within that same record), so there's no parser
//! worth pulling in a dependency for, the same reasoning as `column`.
//! Opened non-blocking so reads stop at `EAGAIN` (the currently
//! buffered messages) instead of following forever like `tail -f`,
//! matching real `dmesg`'s non-`--follow` default.
//!
//! Scope: default output only (`[   12.345678] message`, monotonic
//! seconds.microseconds since boot — the same clock real dmesg's
//! default format uses). Not implemented: `-T`/`--ctime` (wall-clock
//! timestamps), `-l`/`-f` (level/facility filtering), `-c`
//! (clear-after-read), `--follow`, colorized output, JSON/raw formats.
//!
//! Reading `/dev/kmsg` needs `CAP_SYSLOG` (or `CAP_SYS_ADMIN`), gated
//! independently of the device's own file permissions by the
//! `kernel.dmesg_restrict` sysctl (`1` by default on most distros,
//! including this dev machine) — real `dmesg` fails identically
//! unprivileged here (confirmed: `dmesg: read kernel buffer failed:
//! Operation not permitted`, the same message this wraps). Verified
//! for real as root, where an unprivileged sandbox can't reach: booted
//! through `scripts/boot-test.sh`'s real QEMU VM and confirmed a real
//! kernel message (`Linux version ...`) is read back with correct
//! monotonic-timestamp formatting.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::vec::IntoIter;

pub fn run(_args: IntoIter<OsString>) -> i32 {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/kmsg")
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("dmesg: read kernel buffer failed: {}", strerror(&e));
            return 1;
        }
    };
    let mut file = file;

    let mut buf = [0u8; 8192];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => print_record(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            // A record larger than our buffer (rare) comes back
            // truncated with EPIPE/EINVAL on this and future reads at
            // the same position; the kernel kmsg docs call this out
            // explicitly. Not worth chasing for a ring-buffer dump.
            Err(e) => {
                eprintln!("dmesg: error reading /dev/kmsg: {}", strerror(&e));
                return 1;
            }
        }
    }
    0
}

/// `io::Error`'s own `Display` appends " (os error N)" after the
/// message, unlike glibc's plain `strerror()` text real `dmesg` prints
/// — stripped here to match its exact error wording.
fn strerror(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => {
            // SAFETY: strerror returns a pointer to a static/thread-local
            // buffer that's always valid to read as a C string.
            let msg = unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) };
            msg.to_string_lossy().into_owned()
        }
        None => e.to_string(),
    }
}

fn print_record(record: &[u8]) {
    let text = String::from_utf8_lossy(record);
    let Some(header_end) = text.find(';') else {
        return;
    };
    let (header, rest) = text.split_at(header_end);
    let message = rest[1..].lines().next().unwrap_or("");

    let fields: Vec<&str> = header.split(',').collect();
    let timestamp_usec: u64 = fields.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let secs = timestamp_usec / 1_000_000;
    let usecs = timestamp_usec % 1_000_000;

    println!("[{secs:5}.{usecs:06}] {message}");
}
