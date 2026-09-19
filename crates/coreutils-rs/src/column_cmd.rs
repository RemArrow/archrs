//! `column` (from util-linux, a separate project from both
//! `uutils/coreutils` and GNU) — a genuinely simple text-alignment
//! algorithm with no real "engine" worth vendoring, unlike most of
//! what's elsewhere in this crate, so this is a plain from-scratch
//! implementation rather than a vendor-and-wrap.
//!
//! Scope: `-t` (table mode: split each line into fields and align them
//! into columns, matching real `column -t`'s exact spacing — a field
//! width per column, left-justified, joined by two literal spaces,
//! with the last field on each line unpadded), `-s SEP` (input field
//! separator; default is runs of whitespace), reading from files or
//! stdin. Not implemented: `-o` (custom output separator), `-c`
//! (output width / multi-column list mode for non-table input),
//! `-J`/`-N` (JSON/named-column modes).

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::vec::IntoIter;

struct Args {
    table: bool,
    separator: Option<String>,
    files: Vec<String>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut args = Args {
        table: false,
        separator: None,
        files: Vec::new(),
    };
    let mut iter = argv.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-t" | "--table" => args.table = true,
            "-s" | "--separator" => {
                args.separator = Some(iter.next().ok_or("-s requires a separator")?.clone());
            }
            _ if arg.starts_with("-s") && arg.len() > 2 => {
                args.separator = Some(arg[2..].to_string());
            }
            _ if let Some(sep) = arg.strip_prefix("--separator=") => {
                args.separator = Some(sep.to_string());
            }
            _ if arg.starts_with('-') => return Err(format!("unsupported column flag: {arg}")),
            _ => args.files.push(arg.clone()),
        }
    }
    Ok(args)
}

fn split_fields<'a>(line: &'a str, separator: &Option<String>) -> Vec<&'a str> {
    match separator {
        Some(sep) => line.split(sep.as_str()).collect(),
        None => line.split_whitespace().collect(),
    }
}

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let parsed = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("column: {e}");
            return 2;
        }
    };

    let input = if parsed.files.is_empty() {
        let mut buf = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut buf) {
            eprintln!("column: reading stdin: {e}");
            return 1;
        }
        buf
    } else {
        let mut combined = String::new();
        for file in &parsed.files {
            match fs::read_to_string(file) {
                Ok(s) => combined.push_str(&s),
                Err(e) => {
                    eprintln!("column: {file}: {e}");
                    return 1;
                }
            }
        }
        combined
    };

    let rows: Vec<Vec<&str>> = input
        .lines()
        .map(|line| split_fields(line, &parsed.separator))
        .collect();

    if !parsed.table {
        // Without -t, real column packs input into as many display
        // columns as fit the terminal width — not implemented (see
        // module docs); just print each line back out unchanged.
        for line in input.lines() {
            println!("{line}");
        }
        return 0;
    }

    let num_cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; num_cols];
    for row in &rows {
        for (i, field) in row.iter().enumerate() {
            widths[i] = widths[i].max(field.chars().count());
        }
    }

    for row in &rows {
        let mut out = String::new();
        for (i, field) in row.iter().enumerate() {
            if i + 1 == row.len() {
                out.push_str(field);
            } else {
                out.push_str(field);
                let pad = widths[i].saturating_sub(field.chars().count());
                out.push_str(&" ".repeat(pad));
                out.push_str("  ");
            }
        }
        println!("{out}");
    }
    0
}
