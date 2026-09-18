//! Shared helpers for the `crond`/`crontab` binaries — see `crond`'s
//! module doc comment for the scope of this cron implementation.

use std::path::PathBuf;
use std::process::Command;

/// A single-user cron only has one crontab: no `/var/spool/cron/<user>`
/// per-user lookup (that's what "single-user" opts out of — see
/// `crond`'s module docs), no `/etc/crontab`/`cron.d` system tables.
/// `CRON_FILE` overrides it, mainly for testing without touching a
/// real `$HOME`.
pub fn crontab_path() -> PathBuf {
    if let Ok(p) = std::env::var("CRON_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_string());
    PathBuf::from(home).join(".config/archrs/crontab")
}

/// Prefers a sibling `coreutils-rs` binary (dogfooding this project's
/// own `bash`) over the system `sh`, same pattern as `makepkg-rs`.
pub fn shell_command() -> Command {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("coreutils-rs");
        if sibling.is_file() {
            let mut cmd = Command::new(&sibling);
            cmd.arg("sh");
            return cmd;
        }
    }
    Command::new("sh")
}
