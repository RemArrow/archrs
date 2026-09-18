//! A `less`-like pager built on the `minus` crate — an actual terminal-
//! pager library, not something we'd want to hand-roll (raw terminal
//! mode, scroll state, search highlighting). GNU `less` is, like
//! tar/gzip/grep/sed, its own separate upstream project with no
//! `uu_less` to vendor wholesale.
//!
//! Scope: page a file (or stdin) with scrolling and `/`-search, which
//! `minus`'s "static_output" mode provides out of the box. Not
//! implemented: less's own extensive feature set beyond that (multiple
//! files with `:n`/`:p`, marks, filtering, options like `-N` line
//! numbers) — `minus` doesn't expose those, and reimplementing them on
//! top of it is future work, not attempted here.
//!
//! Falls back to printing the content directly (no pager UI) when
//! stdout isn't a terminal, since `minus` needs real terminal control
//! and a non-interactive pipe/redirect has nothing to page through
//! interactively anyway — matches how most pagers behave.

use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::vec::IntoIter;

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let files: Vec<&String> = argv
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();

    let content = if files.is_empty() {
        let mut buf = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut buf) {
            eprintln!("less: reading stdin: {e}");
            return 1;
        }
        buf
    } else {
        match std::fs::read_to_string(files[0]) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("less: {}: {e}", files[0]);
                return 1;
            }
        }
    };

    if !io::stdout().is_terminal() {
        let _ = io::stdout().write_all(content.as_bytes());
        return 0;
    }

    let pager = minus::Pager::new();
    if let Err(e) = pager.set_text(content) {
        eprintln!("less: {e}");
        return 1;
    }
    if let Err(e) = minus::page_all(pager) {
        eprintln!("less: {e}");
        return 1;
    }
    0
}
