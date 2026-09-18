//! `awk`, vendoring the `awk-rs` crate — a real from-scratch AWK
//! lexer/parser/interpreter aiming for POSIX + gawk-extension
//! compatibility. AWK is a full programming language (patterns,
//! actions, control flow, user functions, associative arrays); hand-
//! rolling that from scratch would be its own multi-week project, so
//! this is CLI glue over the crate's public `Lexer`/`Parser`/
//! `Interpreter` types (not re-exported as a ready CLI entry point,
//! only as a binary's `main.rs` we can't call directly) rather than a
//! from-scratch interpreter.
//!
//! Scope: `-F` (field separator), `-v var=val`, `-f progfile`,
//! `-P`/`--posix`, `--traditional`/`-c`, reading the program from the
//! first positional argument or `-f`, and input from positional file
//! arguments or stdin. Whatever AWK-language coverage gaps exist are
//! `awk-rs`'s own (see its README), not audited separately here beyond
//! the spot checks in ROADMAP.md.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::vec::IntoIter;

use awk_rs::{Interpreter, Lexer, Parser};

/// Decodes AWK escape sequences in a `-v name=value` assignment, same
/// as a string literal would be — mirrors the crate's own `main.rs`
/// (not part of its public library API, so duplicated here rather than
/// reused).
fn unescape_assignment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some('n') => {
                chars.next();
                out.push('\n');
            }
            Some('t') => {
                chars.next();
                out.push('\t');
            }
            Some('r') => {
                chars.next();
                out.push('\r');
            }
            Some('\\') => {
                chars.next();
                out.push('\\');
            }
            Some('"') => {
                chars.next();
                out.push('"');
            }
            Some(other) => {
                chars.next();
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();

    let mut field_separator = " ".to_string();
    let mut program_source: Option<String> = None;
    let mut input_files: Vec<String> = Vec::new();
    let mut variables: Vec<(String, String)> = Vec::new();
    let mut posix_mode = false;
    let mut traditional_mode = false;

    let mut iter = argv.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "-P" || arg == "--posix" {
            posix_mode = true;
            traditional_mode = false;
        } else if arg == "-c" || arg == "--traditional" || arg == "--compat" {
            traditional_mode = true;
            posix_mode = false;
        } else if arg == "-F" {
            match iter.next() {
                Some(v) => field_separator = v.clone(),
                None => {
                    eprintln!("awk: -F requires an argument");
                    return 2;
                }
            }
        } else if let Some(fs) = arg.strip_prefix("-F") {
            field_separator = fs.to_string();
        } else if arg == "-v" {
            let Some(assignment) = iter.next() else {
                eprintln!("awk: -v requires an argument");
                return 2;
            };
            match assignment.split_once('=') {
                Some((name, value)) => {
                    variables.push((name.to_string(), unescape_assignment(value)))
                }
                None => {
                    eprintln!("awk: invalid variable assignment: {assignment}");
                    return 2;
                }
            }
        } else if arg == "-f" {
            let Some(path) = iter.next() else {
                eprintln!("awk: -f requires an argument");
                return 2;
            };
            match fs::read_to_string(path) {
                Ok(s) => program_source = Some(s),
                Err(e) => {
                    eprintln!("awk: {path}: {e}");
                    return 2;
                }
            }
        } else if arg == "--" {
            input_files.extend(iter.by_ref().cloned());
        } else if arg.starts_with('-') && arg != "-" {
            eprintln!("awk: unknown option: {arg}");
            return 2;
        } else if program_source.is_none() {
            program_source = Some(arg.clone());
        } else {
            input_files.push(arg.clone());
        }
    }

    let Some(program_source) = program_source else {
        eprintln!("awk: no program given");
        return 2;
    };

    let mut lexer = Lexer::new(&program_source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("awk: {e}");
            return 2;
        }
    };
    let program = match Parser::new(tokens).parse() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("awk: {e}");
            return 2;
        }
    };

    let mut interpreter = Interpreter::new(&program);
    interpreter.set_posix_mode(posix_mode);
    interpreter.set_traditional_mode(traditional_mode);
    interpreter.set_fs(&field_separator);

    let mut awk_argv = vec!["awk".to_string()];
    awk_argv.extend(input_files.iter().cloned());
    interpreter.set_args(awk_argv);

    for (name, value) in &variables {
        interpreter.set_variable(name, value);
    }

    let stdout = io::stdout();
    let mut output = stdout.lock();

    let mut inputs: Vec<Box<dyn BufRead>> = Vec::new();
    let mut filenames: Vec<String> = Vec::new();
    if input_files.is_empty() {
        inputs.push(Box::new(BufReader::new(io::stdin())));
        filenames.push(String::new());
    } else {
        for filename in &input_files {
            if filename == "-" {
                inputs.push(Box::new(BufReader::new(io::stdin())));
            } else {
                match File::open(filename) {
                    Ok(f) => inputs.push(Box::new(BufReader::new(f))),
                    Err(e) => {
                        eprintln!("awk: {filename}: {e}");
                        return 2;
                    }
                }
            }
            filenames.push(filename.clone());
        }
    }
    interpreter.set_filenames(filenames);

    match interpreter.run(inputs, &mut output) {
        Ok(code) => {
            let _ = output.flush();
            code
        }
        Err(e) => {
            eprintln!("awk: {e}");
            2
        }
    }
}
