//! A minimal, single-user cron daemon (ROADMAP.md Phase 5), built on
//! `crontab-rs`'s cron-expression parser/matcher (`Schedule`/`Crontab`
//! — the genuinely fiddly, error-prone part: day-of-month/day-of-week
//! OR semantics, ranges, steps, `@reboot`, etc.) rather than
//! hand-rolling a cron expression evaluator.
//!
//! Deliberately scoped to single-user cron: reads and runs exactly one
//! crontab (see `archrs_cron::crontab_path` — `~/.config/archrs/crontab`
//! by default), running every matched command as whatever user `crond`
//! itself is running as. Real multi-user cron (`/var/spool/cron/<user>`
//! per-user tables, `/etc/crontab`/`cron.d` system tables with a `user`
//! column) needs setuid/setgid privilege-dropping to run each user's
//! jobs as that user, and getting that wrong is a real local-privilege-
//! escalation risk — `crontab-rs` itself only exposes its crontab-
//! parsing/scheduling engine as stable public API precisely because its
//! own daemon/privilege-drop/mail-delivery code is explicitly marked
//! "not covered by semver" internal detail, not something meant for
//! reuse. Rather than reimplementing that security-critical logic
//! ourselves without the scrutiny it deserves, this stays single-user:
//! a real, useful "run my own scheduled jobs" daemon, not a system-wide
//! cron replacement. See ROADMAP.md for the full reasoning.

use std::time::Duration;

use archrs_cron::{crontab_path, shell_command};
use chrono::Local;
use crontab_rs::{Crontab, Format};

fn load_crontab(path: &std::path::Path) -> Option<Crontab> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("crond: {}: {e}", path.display());
            return None;
        }
    };
    match Crontab::parse(&text, Format::User) {
        Ok(c) => Some(c),
        Err(errors) => {
            for e in errors {
                eprintln!("crond: {}: {e:?}", path.display());
            }
            None
        }
    }
}

fn run_entry(command: &[u8]) {
    let command = String::from_utf8_lossy(command).into_owned();
    println!("crond: running: {command}");
    match shell_command().arg("-c").arg(&command).spawn() {
        Ok(mut child) => {
            // Cron jobs run detached — crond moves on to the next
            // minute regardless of how long this one takes, same as
            // real cron. We still reap it (rather than leaking a
            // zombie) once it's done, via a short-lived wait thread.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => eprintln!("crond: failed to start '{command}': {e}"),
    }
}

fn main() {
    let path = crontab_path();
    println!(
        "crond: watching {} (single-user; see module docs)",
        path.display()
    );

    let mut last_checked_minute: Option<String> = None;
    loop {
        let now = Local::now().naive_local();
        let minute_key = now.format("%Y-%m-%d %H:%M").to_string();

        if last_checked_minute.as_deref() != Some(minute_key.as_str()) {
            last_checked_minute = Some(minute_key);
            if let Some(crontab) = load_crontab(&path) {
                for entry in &crontab.entries {
                    if entry.schedule.matches(&now) {
                        run_entry(&entry.command);
                    }
                }
            }
        }

        std::thread::sleep(Duration::from_secs(1));
    }
}
