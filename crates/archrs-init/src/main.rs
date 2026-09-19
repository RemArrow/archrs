//! A minimal PID-1 init for a custom archrs live image (ROADMAP.md Phase
//! 3) — explicitly not a systemd replacement for real installs. Scope:
//! mount the essential pseudo-filesystems, run a small service list from
//! `/etc/archrs-init.conf` (falling back to a rescue shell if none is
//! configured), and reap every child for as long as the system runs,
//! since PID 1 is the reparent target for every orphaned process in the
//! system and nothing else will ever wait() on them.
//!
//! Service list lines take an optional prefix:
//! - (none): start once, don't restart it if it exits.
//! - `wait <cmd>`: start it and block until it exits before moving on to
//!   the next line — the simplest possible ordering primitive, for
//!   one-shot setup steps that later services depend on.
//! - `respawn <cmd>`: start it, and restart it whenever it exits, for as
//!   long as the system isn't shutting down.
//!
//! SIGTERM/SIGINT tear down every process in this PID namespace and
//! exit normally. SIGUSR1/SIGUSR2 do the same teardown but then call the
//! real `reboot(2)` syscall (`RB_AUTOBOOT`/`RB_POWER_OFF`) — there's no
//! separate `reboot`/`poweroff` command here, just these two signals,
//! since this init has no IPC mechanism for anything richer yet. Real
//! reboot(2) is meant to have a genuinely testable effect even outside a
//! real boot: per reboot(2)'s manpage, calling it as the "init" of a
//! non-initial PID namespace terminates that whole namespace instead of
//! the host, and the parent's wait() sees it die by SIGHUP (restart) or
//! SIGINT (power off) — reboot() itself requires CAP_SYS_BOOT, which
//! this sandbox's namespaces don't grant (EPERM), so that specific
//! termination-signal behavior is unverified here; what *is* verified is
//! that the signal handling, shutdown sequence, and the real syscall
//! attempt (with the right mode flag) all run correctly and fail
//! gracefully instead of hanging or panicking. See ROADMAP.md.
//!
//! Testable without a real boot: `unshare --user --pid --mount --fork
//! target/release/archrs-init` creates a fresh PID+mount namespace where
//! this binary genuinely is PID 1, so the mount/spawn/reap/reboot logic
//! below runs for real, not just in theory.

use std::collections::HashMap;
use std::process::Command;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use nix::errno::Errno;
use nix::mount::{MsFlags, mount};
use nix::sys::reboot::{RebootMode, reboot};
use nix::sys::signal::{self, SigHandler, Signal};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::Pid;

const CONFIG_PATH: &str = "/etc/archrs-init.conf";
const FALLBACK_SHELL: &str = "/bin/sh";

const SHUTDOWN_NONE: u8 = 0;
const SHUTDOWN_EXIT: u8 = 1;
const SHUTDOWN_REBOOT: u8 = 2;
const SHUTDOWN_POWEROFF: u8 = 3;

static SHUTDOWN_ACTION: AtomicU8 = AtomicU8::new(SHUTDOWN_NONE);

extern "C" fn handle_exit_signal(_: i32) {
    // Signal handlers may only call async-signal-safe functions; just
    // record the requested action for the reap loop to notice, rather
    // than doing any real teardown work here. First signal wins.
    let _ = SHUTDOWN_ACTION.compare_exchange(
        SHUTDOWN_NONE,
        SHUTDOWN_EXIT,
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
}

extern "C" fn handle_reboot_signal(_: i32) {
    let _ = SHUTDOWN_ACTION.compare_exchange(
        SHUTDOWN_NONE,
        SHUTDOWN_REBOOT,
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
}

extern "C" fn handle_poweroff_signal(_: i32) {
    let _ = SHUTDOWN_ACTION.compare_exchange(
        SHUTDOWN_NONE,
        SHUTDOWN_POWEROFF,
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
}

fn install_signal_handlers() {
    unsafe {
        let _ = signal::signal(Signal::SIGTERM, SigHandler::Handler(handle_exit_signal));
        let _ = signal::signal(Signal::SIGINT, SigHandler::Handler(handle_exit_signal));
        let _ = signal::signal(Signal::SIGUSR1, SigHandler::Handler(handle_reboot_signal));
        let _ = signal::signal(Signal::SIGUSR2, SigHandler::Handler(handle_poweroff_signal));
    }
}

fn mount_pseudo_filesystems() {
    // Make our mount namespace's root private first: without this, a
    // fresh procfs/sysfs/devtmpfs mount here can propagate back out to
    // (or be blocked by) whatever mount namespace we inherited from,
    // which real boot's init never wants.
    let _ = mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    );

    let targets: &[(&str, &str, MsFlags)] = &[
        (
            "proc",
            "/proc",
            MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
        ),
        (
            "sysfs",
            "/sys",
            MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
        ),
        ("devtmpfs", "/dev", MsFlags::MS_NOSUID),
    ];
    for (fstype, target, flags) in targets {
        match mount(Some(*fstype), *target, Some(*fstype), *flags, None::<&str>) {
            Ok(()) => println!("archrs-init: mounted {fstype} on {target}"),
            // EBUSY here means the kernel already auto-mounted this
            // itself before init ran (verified against a real boot:
            // devtmpfs on /dev is commonly kernel-automounted when
            // CONFIG_DEVTMPFS_MOUNT is set) — not a real failure, our
            // own mount would've served the same purpose anyway.
            Err(Errno::EBUSY) => {
                println!("archrs-init: {target} already mounted (kernel auto-mount), skipping");
            }
            Err(e) => eprintln!("archrs-init: could not mount {fstype} on {target}: {e}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceKind {
    Once,
    Wait,
    Respawn,
}

struct ServiceSpec {
    kind: ServiceKind,
    command: String,
}

fn read_service_list() -> Vec<ServiceSpec> {
    // Overridable for testing outside a real boot (see the crate-level
    // doc comment for how this gets exercised under `unshare`) without
    // needing write access to a real system's /etc.
    let path = std::env::var("ARCHRS_INIT_CONF").unwrap_or_else(|_| CONFIG_PATH.to_string());
    match std::fs::read_to_string(&path) {
        Ok(contents) => contents
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|line| {
                if let Some(rest) = line.strip_prefix("wait ") {
                    ServiceSpec {
                        kind: ServiceKind::Wait,
                        command: rest.trim().to_string(),
                    }
                } else if let Some(rest) = line.strip_prefix("respawn ") {
                    ServiceSpec {
                        kind: ServiceKind::Respawn,
                        command: rest.trim().to_string(),
                    }
                } else {
                    ServiceSpec {
                        kind: ServiceKind::Once,
                        command: line.to_string(),
                    }
                }
            })
            .collect(),
        Err(_) => {
            println!("archrs-init: no {path}, falling back to {FALLBACK_SHELL}");
            vec![ServiceSpec {
                kind: ServiceKind::Once,
                command: FALLBACK_SHELL.to_string(),
            }]
        }
    }
}

fn spawn_command(command: &str) -> std::io::Result<std::process::Child> {
    let mut parts = command.split_whitespace();
    let prog = parts
        .next()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty command"))?;
    let cmd_args: Vec<&str> = parts.collect();
    Command::new(prog).args(&cmd_args).spawn()
}

/// Runs the configured service list in order. `wait` entries block this
/// function until they exit — the ordering primitive described in the
/// module docs — before moving on to the next line; `respawn` entries
/// are registered in `respawn` (keyed by pid) so the reap loop can
/// restart them when they exit.
fn spawn_services(respawn: &mut HashMap<i32, String>) {
    for spec in read_service_list() {
        match spec.kind {
            ServiceKind::Wait => match spawn_command(&spec.command) {
                Ok(mut child) => {
                    println!(
                        "archrs-init: waiting for '{}' (pid {})",
                        spec.command,
                        child.id()
                    );
                    match child.wait() {
                        Ok(status) => {
                            println!("archrs-init: '{}' finished ({status})", spec.command)
                        }
                        Err(e) => eprintln!("archrs-init: waiting for '{}': {e}", spec.command),
                    }
                }
                Err(e) => eprintln!("archrs-init: failed to start '{}': {e}", spec.command),
            },
            ServiceKind::Once | ServiceKind::Respawn => match spawn_command(&spec.command) {
                Ok(child) => {
                    println!(
                        "archrs-init: started '{}' (pid {})",
                        spec.command,
                        child.id()
                    );
                    if spec.kind == ServiceKind::Respawn {
                        respawn.insert(child.id() as i32, spec.command.clone());
                    }
                }
                Err(e) => eprintln!("archrs-init: failed to start '{}': {e}", spec.command),
            },
        }
    }
}

/// Sends every other process in this PID namespace SIGTERM, gives them a
/// moment to exit, then SIGKILLs whatever's left.
fn shutdown_children() {
    println!("archrs-init: shutting down — sending SIGTERM to all processes");
    let _ = signal::kill(Pid::from_raw(-1), Signal::SIGTERM);
    std::thread::sleep(Duration::from_millis(500));
    let _ = signal::kill(Pid::from_raw(-1), Signal::SIGKILL);
}

/// If `pid` was a `respawn` service and we're not shutting down, starts
/// a fresh copy of it and re-registers the new pid.
fn maybe_respawn(pid: i32, respawn: &mut HashMap<i32, String>, shutting_down: bool) {
    let Some(command) = respawn.remove(&pid) else {
        return;
    };
    if shutting_down {
        return;
    }
    match spawn_command(&command) {
        Ok(child) => {
            println!("archrs-init: respawning '{command}' (pid {})", child.id());
            respawn.insert(child.id() as i32, command);
        }
        Err(e) => eprintln!("archrs-init: failed to respawn '{command}': {e}"),
    }
}

fn do_final_shutdown_action(action: u8) {
    match action {
        SHUTDOWN_REBOOT => {
            println!("archrs-init: rebooting");
            // reboot(2) returns `Result<Infallible>`: on real success it
            // never returns at all (the machine/namespace is gone), so
            // reaching here at all means it failed.
            let Err(e) = reboot(RebootMode::RB_AUTOBOOT);
            eprintln!("archrs-init: reboot(2) failed: {e}");
        }
        SHUTDOWN_POWEROFF => {
            println!("archrs-init: powering off");
            let Err(e) = reboot(RebootMode::RB_POWER_OFF);
            eprintln!("archrs-init: reboot(2) failed: {e}");
        }
        _ => {}
    }
    println!("archrs-init: no processes left, exiting");
}

/// PID 1 has no parent to reap its own orphaned grandchildren, so any
/// process whose original parent exits first gets reparented to init —
/// without this loop those processes become permanent zombies the
/// moment they exit, since nothing else ever calls wait() on them.
fn reap_loop(respawn: &mut HashMap<i32, String>) {
    let mut shutting_down = false;
    loop {
        let requested = SHUTDOWN_ACTION.load(Ordering::SeqCst);
        if requested != SHUTDOWN_NONE && !shutting_down {
            shutting_down = true;
            shutdown_children();
        }

        match waitpid(Pid::from_raw(-1), None) {
            Ok(WaitStatus::Exited(pid, code)) => {
                println!("archrs-init: reaped pid {pid} (exit code {code})");
                maybe_respawn(pid.as_raw(), respawn, shutting_down);
            }
            Ok(WaitStatus::Signaled(pid, sig, _)) => {
                println!("archrs-init: reaped pid {pid} (killed by {sig})");
                maybe_respawn(pid.as_raw(), respawn, shutting_down);
            }
            Ok(_) => {}
            Err(Errno::ECHILD) => {
                if shutting_down {
                    do_final_shutdown_action(SHUTDOWN_ACTION.load(Ordering::SeqCst));
                    return;
                }
                // Nothing to wait on right now (e.g. every configured
                // service already exited on its own) but we're not
                // shutting down, so just check back shortly.
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(Errno::EINTR) => {}
            Err(e) => {
                eprintln!("archrs-init: waitpid failed: {e}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn main() {
    if std::process::id() != 1 {
        eprintln!(
            "archrs-init: warning: not running as PID 1 (fine under `unshare --user --pid \
             --mount --fork`, but mounting/reaping here won't touch the real system)"
        );
    }

    mount_pseudo_filesystems();
    install_signal_handlers();
    let mut respawn = HashMap::new();
    spawn_services(&mut respawn);
    reap_loop(&mut respawn);
}
