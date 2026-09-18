//! `file`, built on the `magic` crate — real FFI bindings to the
//! system's actual `libmagic` C library and its full, decades-maintained
//! magic database (loaded via `DatabasePaths::default()`, the same
//! default database real `file` uses). A "safe Rust re-implementation
//! of libmagic" crate also exists, but binding the real library gets
//! the exact same detection results real `file` produces, not a
//! second, independently-maintained copy of the magic database that
//! could drift from it — the same reasoning as vendoring `zstd`/
//! `bzip2`/`xz2` against their real C libraries elsewhere in this
//! project rather than reimplementing.
//!
//! Scope: `-i`/`--mime` (MIME type + encoding), `-b`/`--brief` (omit
//! the `filename:` prefix), one or more file arguments. Not
//! implemented: `-` (stdin), directory recursion, `--extension`.

use std::ffi::OsString;
use std::vec::IntoIter;

use magic::Cookie;
use magic::cookie::{DatabasePaths, Flags};

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let mut mime = false;
    let mut brief = false;
    let mut files = Vec::new();

    for arg in argv.iter().skip(1) {
        match arg.as_str() {
            "-i" | "--mime" => mime = true,
            "-b" | "--brief" => brief = true,
            _ if arg.starts_with('-') => {
                eprintln!("file: unsupported flag: {arg}");
                return 2;
            }
            _ => files.push(arg.clone()),
        }
    }

    if files.is_empty() {
        eprintln!("file: no file given");
        return 2;
    }

    let flags = if mime { Flags::MIME } else { Flags::default() };
    let cookie = match Cookie::open(flags) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("file: opening magic database: {e}");
            return 1;
        }
    };
    let cookie = match cookie.load(&DatabasePaths::default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("file: loading magic database: {e}");
            return 1;
        }
    };

    let mut status = 0;
    for path in &files {
        match cookie.file(path) {
            Ok(desc) => {
                if brief {
                    println!("{desc}");
                } else {
                    println!("{path}: {desc}");
                }
            }
            Err(e) => {
                eprintln!("file: {path}: {e}");
                status = 1;
            }
        }
    }
    status
}
