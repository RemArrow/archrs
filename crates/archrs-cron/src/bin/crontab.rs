//! Manages the single-user crontab `crond` reads — see `crond`'s
//! module doc comment for why this is scoped to single-user cron
//! rather than reimplementing multi-user privilege-dropping.
//!
//! Scope: `-l` (print), `-r` (remove), `-e` (edit via `$EDITOR`/`$VISUAL`,
//! validated before being installed — an invalid edit is rejected and
//! the previous crontab is left untouched, same as real `crontab -e`),
//! and installing from a file argument or stdin (`crontab file`,
//! `crontab -` / `crontab` with no args). Every install path validates
//! with `crontab_rs::Crontab::parse` first and refuses to save an
//! invalid crontab. Not implemented: `-u user` (that's exactly the
//! multi-user privilege surface this stays out of).

use std::io::{IsTerminal, Read};
use std::process::{Command, ExitCode};

use archrs_cron::crontab_path;
use crontab_rs::{Crontab, Format};

fn validate(text: &str) -> Result<(), String> {
    match Crontab::parse(text, Format::User) {
        Ok(_) => Ok(()),
        Err(errors) => Err(errors
            .iter()
            .map(|e| format!("{e:?}"))
            .collect::<Vec<_>>()
            .join("\n")),
    }
}

fn cmd_list() -> ExitCode {
    let path = crontab_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("crontab: no crontab for the current user");
            ExitCode::from(1)
        }
    }
}

fn cmd_remove() -> ExitCode {
    let path = crontab_path();
    match std::fs::remove_file(&path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("crontab: no crontab for the current user");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("crontab: {}: {e}", path.display());
            ExitCode::from(1)
        }
    }
}

fn install(text: &str) -> ExitCode {
    if let Err(e) = validate(text) {
        eprintln!("crontab: errors in crontab, not installed:\n{e}");
        return ExitCode::from(1);
    }
    let path = crontab_path();
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!("crontab: creating {}: {e}", parent.display());
        return ExitCode::from(1);
    }
    match std::fs::write(&path, text) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("crontab: writing {}: {e}", path.display());
            ExitCode::from(1)
        }
    }
}

fn cmd_install_from_file(file: &str) -> ExitCode {
    let text = if file == "-" {
        let mut buf = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
            eprintln!("crontab: reading stdin: {e}");
            return ExitCode::from(1);
        }
        buf
    } else {
        match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("crontab: {file}: {e}");
                return ExitCode::from(1);
            }
        }
    };
    install(&text)
}

fn cmd_edit() -> ExitCode {
    let path = crontab_path();
    let original = std::fs::read_to_string(&path).unwrap_or_default();

    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let tmp_path = std::env::temp_dir().join(format!("archrs-crontab-{}.tmp", std::process::id()));
    if let Err(e) = std::fs::write(&tmp_path, &original) {
        eprintln!("crontab: creating temp file: {e}");
        return ExitCode::from(1);
    }

    let status = Command::new(&editor).arg(&tmp_path).status();
    let edited = std::fs::read_to_string(&tmp_path).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp_path);

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("crontab: {editor} exited with {s}, not installing");
            return ExitCode::from(1);
        }
        Err(e) => {
            eprintln!("crontab: running {editor}: {e}");
            return ExitCode::from(1);
        }
    }

    if edited == original {
        println!("crontab: no changes made");
        return ExitCode::SUCCESS;
    }
    if let Err(e) = validate(&edited) {
        eprintln!("crontab: errors in crontab, not installed:\n{e}");
        eprintln!("crontab: the previous crontab is unchanged");
        return ExitCode::from(1);
    }
    install(&edited)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("-l") => cmd_list(),
        Some("-r") => cmd_remove(),
        Some("-e") => cmd_edit(),
        Some("-u") => {
            eprintln!(
                "crontab: -u (other users' crontabs) is not supported — see crond's module docs"
            );
            ExitCode::from(1)
        }
        Some("-") => cmd_install_from_file("-"),
        Some(file) if !file.starts_with('-') => cmd_install_from_file(file),
        None => {
            if std::io::stdin().is_terminal() {
                eprintln!("usage: crontab -l | -r | -e | file | -");
                ExitCode::from(1)
            } else {
                cmd_install_from_file("-")
            }
        }
        Some(other) => {
            eprintln!("crontab: unsupported option: {other}");
            ExitCode::from(1)
        }
    }
}
