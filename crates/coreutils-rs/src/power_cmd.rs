//! `reboot`/`poweroff`/`halt`/`shutdown` — the user-facing side of
//! `archrs-init`'s own shutdown mechanism. `archrs-init` (Phase 3) only
//! ever grew signal-based shutdown (SIGTERM/SIGINT to exit, SIGUSR1 to
//! reboot, SIGUSR2 to power off — see its own module doc comment for why:
//! "no IPC mechanism for anything richer yet"), verified so far only by
//! sending those signals directly from outside the VM (`kill -USR2 1` in
//! `scripts/boot-test.sh`). That's not something an actual logged-in user
//! can do — these commands are the missing other half: real, minimal
//! wrappers that just send the same real signals to PID 1, matching
//! `archrs-init`'s own real (not simulated) mechanism exactly rather than
//! inventing a second one.
//!
//! Real `reboot`/`poweroff`/`halt` on other distros go through
//! `systemd`/D-Bus or a SysV `/dev/initctl` FIFO — neither exists here,
//! since `archrs-init` deliberately isn't a systemd replacement. This is
//! the right level of implementation for *this* init, not a stand-in for
//! a mechanism this project isn't building.
//!
//! `kill(1, ...)` itself enforces the real permission check (matching
//! signal's own permission model: sender's real/effective uid must match
//! PID 1's, i.e. be root, or the kernel returns EPERM) — no separate
//! privilege check needed here.
//!
//! Not implemented: real delayed shutdown (`shutdown +5`, wall messages
//! to other sessions), `-f`/`--force`, `--no-wall`. `shutdown`'s only
//! recognized forms are `shutdown [-r|-h] now` and bare `shutdown`
//! (power off), matching the two signals `archrs-init` actually handles.

use std::ffi::OsString;
use std::vec::IntoIter;

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;

fn signal_init(signal: Signal, action: &str) -> i32 {
    match self::signal::kill(Pid::from_raw(1), signal) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{action}: could not signal PID 1: {e}");
            1
        }
    }
}

pub fn run_reboot(_args: IntoIter<OsString>) -> i32 {
    signal_init(Signal::SIGUSR1, "reboot")
}

pub fn run_poweroff(_args: IntoIter<OsString>) -> i32 {
    signal_init(Signal::SIGUSR2, "poweroff")
}

pub fn run_shutdown(mut args: IntoIter<OsString>) -> i32 {
    args.next(); // argv[0]: our own utility name
    let reboot = args.any(|a| a == "-r");
    if reboot {
        signal_init(Signal::SIGUSR1, "shutdown")
    } else {
        signal_init(Signal::SIGUSR2, "shutdown")
    }
}
