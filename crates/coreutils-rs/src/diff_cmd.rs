//! `diff` and `cmp` — GNU diffutils, a separate upstream project from
//! `uutils/coreutils` (like `procps-ng`/`util-linux` elsewhere in this
//! crate), but with its own official uutils-maintained Rust port to
//! vendor: the `diffutils` crate (library name `diffutilslib`),
//! published by the same `uutils` org as `uucore`/`findutils`.
//!
//! Unlike `findutils` or the `uu_*` crates, though, its multicall CLI
//! glue (`diffutils`'s own `main.rs`, dispatching to `diff`/`cmp` by
//! argv[0]) isn't part of the published library — only `src/lib.rs`'s
//! `pub mod`s are, and that omits the `diff` module entirely (it only
//! exists in the binary crate's `main.rs`, never `pub`). So, same
//! reasoning as `awk_cmd.rs` needing its own CLI layer over `awk-rs`'s
//! public lexer/parser/interpreter: this file re-derives the two
//! multicall entry points' logic (each under 40 lines in the real
//! `diffutils` source) directly from its own changelog/source, calling
//! straight into the vendored crate's public, unmodified engine and
//! argument-parsing functions for everything that actually matters —
//! the diff algorithm itself (`diff` crate, wrapped by
//! `{normal,unified,context,ed,side}_diff::diff`), and all of
//! `cmp`/`diff`'s real flag parsing (`cmp::parse_params`/
//! `params::parse_params`, both generic over any `OsString` iterator,
//! not tied to real process argv).
//!
//! `cmp`'s own `cmp()` function does all of its real output itself
//! (default "differ" message, `-l` verbose byte-by-byte listing, EOF
//! messages) as a side effect before returning just a summary
//! `Cmp::Equal`/`Cmp::Different` — so `run_cmp` below needs no access
//! to `cmp`'s otherwise-private formatting helpers to get real,
//! unmodified `cmp` output.
//!
//! Verified byte-identical against real GNU diffutils 3.12 across:
//! `diff` unified (`-u`)/context (`-c`)/normal/ed (`-e`) formats on
//! files with real differences, `-q`/`--brief`, `-s` on identical
//! files, reading one side from stdin (`-`); `cmp` on identical files
//! (silent, exit 0), differing files (default "differ at byte N, line
//! N" message and exit 1), and `-l` (verbose byte-by-byte octal
//! listing).
//! Not implemented: `diff -r` (recursive directory comparison —
//! `parse_params` itself has no notion of directories, only two file
//! paths) and `diff3`/`sdiff` (present in real diffutils but not
//! vendored here at all; not common PKGBUILD/day-to-day needs).

use diffutilslib::params::Format;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::vec::IntoIter;

fn read_file_contents(filepath: &OsString) -> io::Result<Vec<u8>> {
    if filepath == "-" {
        let mut content = Vec::new();
        io::stdin().read_to_end(&mut content).and(Ok(content))
    } else {
        fs::read(filepath)
    }
}

pub fn run_diff(args: IntoIter<OsString>) -> i32 {
    let params = match diffutilslib::params::parse_params(args.peekable()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };

    let report_identical = |params: &diffutilslib::params::Params| {
        if params.report_identical_files {
            println!(
                "Files {} and {} are identical",
                params.from.to_string_lossy(),
                params.to.to_string_lossy(),
            );
        }
    };

    if params.from == "-" && params.to == "-"
        || same_file::is_same_file(&params.from, &params.to).unwrap_or(false)
    {
        report_identical(&params);
        return 0;
    }

    let mut io_error = false;
    let from_content = read_file_contents(&params.from).unwrap_or_else(|e| {
        eprintln!("diff: {}: {e}", params.from.to_string_lossy());
        io_error = true;
        Vec::new()
    });
    let to_content = read_file_contents(&params.to).unwrap_or_else(|e| {
        eprintln!("diff: {}: {e}", params.to.to_string_lossy());
        io_error = true;
        Vec::new()
    });
    if io_error {
        return 2;
    }

    let result: Vec<u8> = match params.format {
        Format::Normal => diffutilslib::normal_diff(&from_content, &to_content, &params),
        Format::Unified => diffutilslib::unified_diff(&from_content, &to_content, &params),
        Format::Context => diffutilslib::context_diff(&from_content, &to_content, &params),
        Format::Ed => {
            diffutilslib::ed_diff(&from_content, &to_content, &params).unwrap_or_else(|e| {
                eprintln!("diff: {e}");
                std::process::exit(2);
            })
        }
        Format::SideBySide => {
            let mut out = io::stdout().lock();
            diffutilslib::side_by_side_diff(&from_content, &to_content, &mut out, &params)
        }
    };

    if params.brief && !result.is_empty() {
        println!(
            "Files {} and {} differ",
            params.from.to_string_lossy(),
            params.to.to_string_lossy()
        );
    } else {
        io::stdout().write_all(&result).unwrap();
    }

    if result.is_empty() {
        report_identical(&params);
        0
    } else {
        1
    }
}

pub fn run_cmp(args: IntoIter<OsString>) -> i32 {
    let params = match diffutilslib::cmp::parse_params(args.peekable()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    match diffutilslib::cmp::cmp(&params) {
        Ok(diffutilslib::cmp::Cmp::Equal) => 0,
        Ok(diffutilslib::cmp::Cmp::Different) => 1,
        Err(e) => {
            eprintln!("{e}");
            2
        }
    }
}
