//! A minimal PID-1 init for a custom archrs live image (ROADMAP.md Phase
//! 3) — explicitly not a systemd replacement for real installs. Scope:
//! mount the essential pseudo-filesystems, spawn a small fixed list of
//! services (falling back to a rescue shell if none are configured), and
//! reap every child for as long as the system runs, since PID 1 is the
//! reparent target for every orphaned process in the system and nothing
//! else will ever wait() on them.
//!
//! Out of scope for this first pass: real reboot(2)/poweroff(2) syscall
//! orchestration (syncing and unmounting filesystems, actually power-
//! cycling hardware) — SIGTERM/SIGINT here just tear down every process
//! in this PID namespace and exit, which is what's actually verifiable
//! outside a real boot anyway. See ROADMAP.md for what's still open.
//!
//! Testable without a real boot: `unshare --user --pid --mount --fork
//! target/release/archrs-init` creates a fresh PID+mount namespace where
//! this binary genuinely is PID 1, so the mount/spawn/reap logic below
//! runs for real, not just in theory.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nix::errno::Errno;
use nix::mount::{MsFlags, mount};
use nix::sys::signal::{self, SigHandler, Signal};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::Pid;

const CONFIG_PATH: &str = "/etc/archrs-init.conf";
const FALLBACK_SHELL: &str = "/bin/sh";

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_shutdown_signal(_: i32) {
    // Signal handlers may only call async-signal-safe functions; just
    // flip a flag for the reap loop to notice, rather than doing any
    // real teardown work here.
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    let handler = SigHandler::Handler(handle_shutdown_signal);
    unsafe {
        let _ = signal::signal(Signal::SIGTERM, handler);
        let _ = signal::signal(Signal::SIGINT, handler);
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
            Err(e) => eprintln!("archrs-init: could not mount {fstype} on {target}: {e}"),
        }
    }
}

fn read_service_list() -> Vec<String> {
    // Overridable for testing outside a real boot (see the crate-level
    // doc comment for how this gets exercised under `unshare`) without
    // needing write access to a real system's /etc.
    let path = std::env::var("ARCHRS_INIT_CONF").unwrap_or_else(|_| CONFIG_PATH.to_string());
    match std::fs::read_to_string(&path) {
        Ok(contents) => contents
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect(),
        Err(_) => {
            println!("archrs-init: no {path}, falling back to {FALLBACK_SHELL}");
            vec![FALLBACK_SHELL.to_string()]
        }
    }
}

fn spawn_services() -> Vec<std::process::Child> {
    let mut children = Vec::new();
    for line in read_service_list() {
        let mut parts = line.split_whitespace();
        let Some(prog) = parts.next() else { continue };
        let cmd_args: Vec<&str> = parts.collect();
        match Command::new(prog).args(&cmd_args).spawn() {
            Ok(child) => {
                println!("archrs-init: started '{line}' (pid {})", child.id());
                children.push(child);
            }
            Err(e) => eprintln!("archrs-init: failed to start '{line}': {e}"),
        }
    }
    children
}

/// Sends every other process in this PID namespace SIGTERM, gives them a
/// moment to exit, then SIGKILLs whatever's left.
fn shutdown_children() {
    println!("archrs-init: shutting down — sending SIGTERM to all processes");
    let _ = signal::kill(Pid::from_raw(-1), Signal::SIGTERM);
    std::thread::sleep(Duration::from_millis(500));
    let _ = signal::kill(Pid::from_raw(-1), Signal::SIGKILL);
}

/// PID 1 has no parent to reap its own orphaned grandchildren, so any
/// process whose original parent exits first gets reparented to init —
/// without this loop those processes become permanent zombies the
/// moment they exit, since nothing else ever calls wait() on them.
fn reap_loop() {
    let mut shutting_down = false;
    loop {
        if SHUTDOWN_REQUESTED.load(Ordering::SeqCst) && !shutting_down {
            shutting_down = true;
            shutdown_children();
        }

        match waitpid(Pid::from_raw(-1), None) {
            Ok(WaitStatus::Exited(pid, code)) => {
                println!("archrs-init: reaped pid {pid} (exit code {code})");
            }
            Ok(WaitStatus::Signaled(pid, sig, _)) => {
                println!("archrs-init: reaped pid {pid} (killed by {sig})");
            }
            Ok(_) => {}
            Err(Errno::ECHILD) => {
                if shutting_down {
                    println!("archrs-init: no processes left, exiting");
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
    let _children = spawn_services();
    reap_loop();
}
