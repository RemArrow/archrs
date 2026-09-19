//! `reboot`/`poweroff`/`halt`/`shutdown` — thin wrappers around real
//! `systemctl reboot`/`poweroff`/`halt`, this project's real init as of
//! Phase 13 (systemd replaced the project's own `archrs-init`; see
//! ROADMAP.md's Phase 13 section for why — the initramfs this project
//! already relies on for a real bootable install assumes systemd for
//! early boot regardless, and using it for the full system too matches
//! how real Arch/Manjaro actually boot, consistent with this project's
//! established pattern of using real supporting infrastructure — real
//! kernel, real GRUB, real PAM — rather than reinventing it).
//!
//! `systemctl` itself does the real privilege enforcement here (via
//! polkit when running under a session, or trivially for root) — no
//! separate check needed in this wrapper, the same "let the real
//! mechanism's own permission model do the work" principle this
//! project already applied to `su`/`sudo` (`kill(2)`'s own permission
//! check) before systemd replaced signal-to-PID-1 as the real
//! mechanism.
//!
//! Not implemented: real delayed shutdown (`shutdown +5`, wall
//! messages), `-f`/`--force`, `--no-wall`, `kexec`/`soft-reboot`/
//! `suspend`/`hibernate` (all real `systemctl` verbs, just not wired
//! up here). `shutdown`'s only recognized forms are `shutdown [-r|-h]
//! now` and bare `shutdown` (power off).

use std::ffi::OsString;
use std::process::Command;
use std::vec::IntoIter;

fn run_systemctl(verb: &str, action: &str) -> i32 {
    match Command::new("systemctl").arg(verb).status() {
        Ok(status) if status.success() => 0,
        Ok(status) => {
            eprintln!("{action}: systemctl {verb} exited with {status}");
            status.code().unwrap_or(1)
        }
        Err(e) => {
            eprintln!("{action}: could not run systemctl: {e}");
            1
        }
    }
}

pub fn run_reboot(_args: IntoIter<OsString>) -> i32 {
    run_systemctl("reboot", "reboot")
}

pub fn run_poweroff(_args: IntoIter<OsString>) -> i32 {
    run_systemctl("poweroff", "poweroff")
}

pub fn run_halt(_args: IntoIter<OsString>) -> i32 {
    run_systemctl("halt", "halt")
}

pub fn run_shutdown(mut args: IntoIter<OsString>) -> i32 {
    args.next(); // argv[0]: our own utility name
    let reboot = args.any(|a| a == "-r");
    if reboot {
        run_systemctl("reboot", "shutdown")
    } else {
        run_systemctl("poweroff", "shutdown")
    }
}
