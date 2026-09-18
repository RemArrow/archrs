//! A GNU-sed-compatible CLI, deliberately scoped to a useful subset of
//! sed's scripting language rather than the whole thing. GNU sed's full
//! language (hold space, branches/labels, multi-line commands, `y///`,
//! `a`/`i`/`c`) has no existing Rust implementation to draw on, unlike
//! everything else vendored elsewhere in this crate — reimplementing all
//! of it from scratch is a much bigger undertaking than the
//! "adapt/vendor where sensible" pattern the rest of this repo follows,
//! so this covers the substitution/filtering subset that accounts for
//! the overwhelming majority of everyday one-liner sed usage instead.
//!
//! Supported:
//! - addresses: a line number, `$` (last line), `/regex/`, and
//!   `addr1,addr2` ranges, each optionally negated with a trailing `!`
//! - commands: `s/pattern/replacement/flags` (flags `g`, `i`/`I`, `p`;
//!   any non-backslash character may be used as the delimiter, not just
//!   `/`), `p`, `d`, `q`
//! - `-n` (suppress default auto-print), `-e script` (repeatable),
//!   `-i[SUFFIX]` (in-place edit, optional backup suffix)
//! - replacement text: `&` for the whole match, `\1`..`\9` for capture
//!   groups, `\&` and `\\` for literal `&`/`\`
//!
//! Not implemented: hold space (`h`/`H`/`g`/`G`/`x`), branches/labels
//! (`b`/`t`/`:`), multi-line commands (`N`/`D`/`P`), `y///`, `a`/`i`/`c`,
//! and GNU sed's many other extensions. Whole input is read into memory
//! rather than streamed, which is fine at the scale this is meant for.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::vec::IntoIter;

use regex::Regex;

#[derive(Debug, Clone)]
enum Address {
    Line(usize),
    Last,
    Regex(Regex),
}

impl Address {
    fn matches(&self, line_no: usize, is_last: bool, text: &str) -> bool {
        match self {
            Address::Line(n) => *n == line_no,
            Address::Last => is_last,
            Address::Regex(re) => re.is_match(text),
        }
    }
}

#[derive(Debug, Clone)]
struct Range {
    start: Address,
    end: Option<Address>,
}

#[derive(Debug, Clone)]
enum Command {
    Substitute {
        pattern: Regex,
        replacement: String,
        global: bool,
        print: bool,
    },
    Print,
    Delete,
    Quit,
}

struct ScriptCommand {
    range: Option<Range>,
    negate: bool,
    command: Command,
    // Whether the range (if any) is currently "open" — needed across
    // lines for addr1,addr2-style ranges.
    active: bool,
}

fn parse_error(msg: impl Into<String>) -> String {
    format!("sed: {}", msg.into())
}

/// Reads a `/regex/`-delimited address starting just after the opening
/// `/`. Returns the compiled regex and the byte offset just past the
/// closing (unescaped) `/`.
fn parse_regex_address(s: &str) -> Result<(Regex, usize), String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut pattern = String::new();
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                pattern.push(bytes[i] as char);
                pattern.push(bytes[i + 1] as char);
                i += 2;
            }
            b'/' => {
                let re = Regex::new(&pattern).map_err(|e| parse_error(e.to_string()))?;
                return Ok((re, i + 1));
            }
            c => {
                pattern.push(c as char);
                i += 1;
            }
        }
    }
    Err(parse_error("unterminated address regex"))
}

fn parse_address(s: &str) -> Result<(Address, usize), String> {
    let bytes = s.as_bytes();
    if bytes.first() == Some(&b'$') {
        return Ok((Address::Last, 1));
    }
    if bytes.first() == Some(&b'/') {
        let (re, consumed) = parse_regex_address(&s[1..])?;
        return Ok((Address::Regex(re), consumed + 1));
    }
    if bytes.first().is_some_and(u8::is_ascii_digit) {
        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let n: usize = s[..end]
            .parse()
            .map_err(|_| parse_error("bad line number"))?;
        return Ok((Address::Line(n), end));
    }
    Err(parse_error(format!("expected an address, got '{s}'")))
}

/// Reads a delimiter-terminated field of an `s///`-style command,
/// unescaping `\<delim>` into a literal delimiter and leaving every
/// other backslash sequence untouched (the caller decides what those
/// mean — regex escapes for the pattern, `\1`/`\&`/`\\` for the
/// replacement).
fn read_delimited(s: &str, delim: u8) -> Result<(String, usize), String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut field = String::new();
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            if bytes[i + 1] == delim {
                field.push(delim as char);
            } else {
                field.push('\\');
                field.push(bytes[i + 1] as char);
            }
            i += 2;
        } else if bytes[i] == delim {
            return Ok((field, i + 1));
        } else {
            field.push(bytes[i] as char);
            i += 1;
        }
    }
    Err(parse_error("unterminated s/// command"))
}

/// Translates sed's `&`/`\1`../`\&`/`\\` replacement syntax into the
/// `regex` crate's own `${0}`/`${1}`/literal syntax, so `Regex::replace`
/// can be used directly instead of hand-rolling capture substitution.
fn translate_replacement(sed_repl: &str) -> String {
    let bytes = sed_repl.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() => {
                out.push_str(&format!("${{{}}}", bytes[i + 1] as char));
                i += 2;
            }
            b'\\' if i + 1 < bytes.len() && (bytes[i + 1] == b'&' || bytes[i + 1] == b'\\') => {
                out.push(bytes[i + 1] as char);
                i += 2;
            }
            b'&' => {
                out.push_str("${0}");
                i += 1;
            }
            b'$' => {
                out.push_str("$$");
                i += 1;
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

fn parse_command(s: &str) -> Result<(ScriptCommand, usize), String> {
    let mut pos = 0;
    let rest = s.trim_start_matches([' ', '\t']);
    pos += s.len() - rest.len();

    let range = if !rest.is_empty()
        && (rest.as_bytes()[0] == b'$'
            || rest.as_bytes()[0] == b'/'
            || rest.as_bytes()[0].is_ascii_digit())
    {
        let (start, consumed) = parse_address(&s[pos..])?;
        pos += consumed;
        let mut end = None;
        if s[pos..].starts_with(',') {
            pos += 1;
            let (e, consumed) = parse_address(&s[pos..])?;
            pos += consumed;
            end = Some(e);
        }
        Some(Range { start, end })
    } else {
        None
    };

    let after_addr = s[pos..].trim_start_matches([' ', '\t']);
    pos += s[pos..].len() - after_addr.len();

    let mut negate = false;
    if s[pos..].starts_with('!') {
        negate = true;
        pos += 1;
    }
    let after_bang = s[pos..].trim_start_matches([' ', '\t']);
    pos += s[pos..].len() - after_bang.len();

    let Some(&cmd_byte) = s.as_bytes().get(pos) else {
        return Err(parse_error("expected a command"));
    };
    pos += 1;

    let command = match cmd_byte {
        b's' => {
            let Some(&delim) = s.as_bytes().get(pos) else {
                return Err(parse_error("s command missing delimiter"));
            };
            pos += 1;
            let (pattern_src, consumed) = read_delimited(&s[pos..], delim)?;
            pos += consumed;
            let (replacement_src, consumed) = read_delimited(&s[pos..], delim)?;
            pos += consumed;

            let mut global = false;
            let mut ignore_case = false;
            let mut print = false;
            while let Some(&c) = s.as_bytes().get(pos) {
                match c {
                    b'g' => global = true,
                    b'i' | b'I' => ignore_case = true,
                    b'p' => print = true,
                    _ => break,
                }
                pos += 1;
            }

            let pattern_final = if ignore_case {
                format!("(?i){pattern_src}")
            } else {
                pattern_src
            };
            let pattern = Regex::new(&pattern_final).map_err(|e| parse_error(e.to_string()))?;
            Command::Substitute {
                pattern,
                replacement: translate_replacement(&replacement_src),
                global,
                print,
            }
        }
        b'p' => Command::Print,
        b'd' => Command::Delete,
        b'q' => Command::Quit,
        other => {
            return Err(parse_error(format!(
                "unsupported command: {}",
                other as char
            )));
        }
    };

    Ok((
        ScriptCommand {
            range,
            negate,
            command,
            active: false,
        },
        pos,
    ))
}

fn parse_script(script: &str) -> Result<Vec<ScriptCommand>, String> {
    let mut commands = Vec::new();
    for line in script.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (cmd, _consumed) = parse_command(line)?;
        commands.push(cmd);
    }
    Ok(commands)
}

fn range_matches(cmd: &mut ScriptCommand, line_no: usize, is_last: bool, text: &str) -> bool {
    let Some(range) = &cmd.range else {
        return true;
    };
    let matched = if let Some(end) = &range.end {
        if cmd.active {
            let should_close = end.matches(line_no, is_last, text)
                || matches!(end, Address::Line(n) if *n <= line_no);
            if should_close {
                cmd.active = false;
            }
            true
        } else if range.start.matches(line_no, is_last, text) {
            cmd.active = true;
            // A range can close on the very line it opens (e.g. the end
            // address is a line number equal to the start).
            if matches!(end, Address::Line(n) if *n <= line_no) {
                cmd.active = false;
            }
            true
        } else {
            false
        }
    } else {
        range.start.matches(line_no, is_last, text)
    };
    matched != cmd.negate
}

fn run_script(commands: &mut [ScriptCommand], input: &str, suppress_auto_print: bool) -> String {
    let lines: Vec<&str> = input.lines().collect();
    let mut out = String::new();
    let total = lines.len();

    for (idx, &line) in lines.iter().enumerate() {
        let line_no = idx + 1;
        let is_last = line_no == total;
        let mut text = line.to_string();
        let mut deleted = false;
        let mut quit = false;

        for cmd in commands.iter_mut() {
            if !range_matches(cmd, line_no, is_last, &text) {
                continue;
            }
            match &cmd.command {
                Command::Substitute {
                    pattern,
                    replacement,
                    global,
                    print,
                } => {
                    let (new_text, did_replace) = if *global {
                        let replaced = pattern.replace_all(&text, replacement.as_str());
                        let did = matches!(&replaced, std::borrow::Cow::Owned(_));
                        (replaced.into_owned(), did)
                    } else {
                        let replaced = pattern.replace(&text, replacement.as_str());
                        let did = matches!(&replaced, std::borrow::Cow::Owned(_));
                        (replaced.into_owned(), did)
                    };
                    text = new_text;
                    if did_replace && *print {
                        out.push_str(&text);
                        out.push('\n');
                    }
                }
                Command::Print => {
                    out.push_str(&text);
                    out.push('\n');
                }
                Command::Delete => {
                    deleted = true;
                    break;
                }
                Command::Quit => {
                    quit = true;
                    break;
                }
            }
        }

        if !deleted && !suppress_auto_print {
            out.push_str(&text);
            out.push('\n');
        }
        if quit {
            break;
        }
    }

    out
}

struct Args {
    suppress_auto_print: bool,
    in_place: Option<String>,
    scripts: Vec<String>,
    files: Vec<String>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut args = Args {
        suppress_auto_print: false,
        in_place: None,
        scripts: Vec::new(),
        files: Vec::new(),
    };
    let mut iter = argv.iter().skip(1).peekable();
    let mut explicit_script_given = false;

    while let Some(arg) = iter.next() {
        if let Some(suffix) = arg.strip_prefix("-i") {
            args.in_place = Some(suffix.to_string());
        } else if arg == "-n" || arg == "--quiet" || arg == "--silent" {
            args.suppress_auto_print = true;
        } else if arg == "-e" || arg == "--expression" {
            let script = iter.next().ok_or("sed: -e requires a script")?;
            args.scripts.push(script.clone());
            explicit_script_given = true;
        } else if arg == "--regexp-extended" || arg == "--posix" {
            // No-op: the regex crate is already ERE-like by default (see
            // module docs), so there's no separate BRE/ERE mode to
            // switch between.
        } else if let Some(bundle) = arg.strip_prefix('-').filter(|s| !s.starts_with('-')) {
            for c in bundle.chars() {
                match c {
                    'n' => args.suppress_auto_print = true,
                    'E' | 'r' => {} // extended-regexp: no-op, see module docs
                    other => return Err(format!("unsupported sed flag: -{other}")),
                }
            }
        } else if !explicit_script_given && args.scripts.is_empty() {
            args.scripts.push(arg.clone());
            explicit_script_given = true;
        } else {
            args.files.push(arg.clone());
        }
    }

    Ok(args)
}

fn run_on_string(commands: &mut [ScriptCommand], input: &str, suppress: bool) -> String {
    for cmd in commands.iter_mut() {
        cmd.active = false;
    }
    run_script(commands, input, suppress)
}

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let parsed = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if parsed.scripts.is_empty() {
        eprintln!("sed: no script given");
        return 1;
    }
    let script = parsed.scripts.join("\n");
    let mut commands = match parse_script(&script) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };

    if parsed.files.is_empty() {
        let mut input = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut input) {
            eprintln!("sed: reading stdin: {e}");
            return 1;
        }
        let output = run_on_string(&mut commands, &input, parsed.suppress_auto_print);
        let _ = io::stdout().write_all(output.as_bytes());
        return 0;
    }

    let mut status = 0;
    for file in &parsed.files {
        let input = match fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("sed: {file}: {e}");
                status = 1;
                continue;
            }
        };
        let output = run_on_string(&mut commands, &input, parsed.suppress_auto_print);
        match &parsed.in_place {
            Some(suffix) => {
                if !suffix.is_empty()
                    && let Err(e) = fs::copy(file, format!("{file}{suffix}"))
                {
                    eprintln!("sed: backing up {file}: {e}");
                    status = 1;
                    continue;
                }
                if let Err(e) = fs::write(file, output) {
                    eprintln!("sed: writing {file}: {e}");
                    status = 1;
                }
            }
            None => {
                let _ = io::stdout().write_all(output.as_bytes());
            }
        }
    }
    status
}
