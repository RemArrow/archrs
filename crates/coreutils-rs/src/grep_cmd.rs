//! A GNU-grep-compatible CLI built on ripgrep's own search-engine
//! libraries (`grep-matcher`/`grep-regex`/`grep-searcher`) — the actual
//! libraries ripgrep itself is built from, vendored the same way
//! `alpm-rs` vendors `tar`/`flate2` rather than reimplementing a regex
//! search engine from scratch. No `uu_grep` exists to vendor wholesale
//! (GNU grep is a separate upstream project, not part of
//! `uutils/coreutils`), so the CLI-flag parsing and output formatting
//! below are real glue code, not just a Cargo.toml entry.
//!
//! Scope: `-i -v -n -c -l -L -r -R -w -x -o -F -E -H -h`, reading from
//! files, directories (recursively with `-r`/`-R`), or stdin.
//!
//! Pattern syntax is the `regex` crate's own (roughly ERE-like, no
//! backreferences) rather than true POSIX BRE/ERE — GNU grep's default
//! mode is BRE, which has different escaping conventions (`\(` `\)` for
//! groups) the regex crate doesn't support. Most everyday patterns
//! (literal text, character classes, `*`, `.`, `^`, `$`) behave
//! identically either way; patterns relying on BRE-specific escaping
//! don't. `-E` is accepted but has no separate effect, since the
//! underlying engine is already ERE-like by default.
//!
//! Not implemented: `-A`/`-B`/`-C` context lines, `-P` (PCRE), and
//! locale-aware matching.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::vec::IntoIter;

use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::SearcherBuilder;
use grep_searcher::sinks::UTF8;

struct Args {
    ignore_case: bool,
    invert: bool,
    line_number: bool,
    count: bool,
    files_with_matches: bool,
    files_without_match: bool,
    recursive: bool,
    word_regexp: bool,
    line_regexp: bool,
    only_matching: bool,
    fixed_strings: bool,
    with_filename: Option<bool>,
    pattern: Option<String>,
    paths: Vec<PathBuf>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut args = Args {
        ignore_case: false,
        invert: false,
        line_number: false,
        count: false,
        files_with_matches: false,
        files_without_match: false,
        recursive: false,
        word_regexp: false,
        line_regexp: false,
        only_matching: false,
        fixed_strings: false,
        with_filename: None,
        pattern: None,
        paths: Vec::new(),
    };

    for arg in argv.iter().skip(1) {
        if let Some(bundle) = arg.strip_prefix('-').filter(|s| !s.starts_with('-')) {
            for c in bundle.chars() {
                match c {
                    'i' => args.ignore_case = true,
                    'v' => args.invert = true,
                    'n' => args.line_number = true,
                    'c' => args.count = true,
                    'l' => args.files_with_matches = true,
                    'L' => args.files_without_match = true,
                    'r' | 'R' => args.recursive = true,
                    'w' => args.word_regexp = true,
                    'x' => args.line_regexp = true,
                    'o' => args.only_matching = true,
                    'F' => args.fixed_strings = true,
                    'E' | 'e' => {} // -E: no-op (see module docs); -e handled below as a bundle char is unusual, ignored
                    'H' => args.with_filename = Some(true),
                    'h' => args.with_filename = Some(false),
                    other => return Err(format!("unsupported grep flag: -{other}")),
                }
            }
            continue;
        }

        match arg.as_str() {
            "--ignore-case" => args.ignore_case = true,
            "--invert-match" => args.invert = true,
            "--line-number" => args.line_number = true,
            "--count" => args.count = true,
            "--files-with-matches" => args.files_with_matches = true,
            "--files-without-match" => args.files_without_match = true,
            "--recursive" => args.recursive = true,
            "--word-regexp" => args.word_regexp = true,
            "--line-regexp" => args.line_regexp = true,
            "--only-matching" => args.only_matching = true,
            "--fixed-strings" => args.fixed_strings = true,
            "--extended-regexp" => {}
            "--with-filename" => args.with_filename = Some(true),
            "--no-filename" => args.with_filename = Some(false),
            _ if arg.starts_with('-') => return Err(format!("unsupported grep flag: {arg}")),
            _ if args.pattern.is_none() => args.pattern = Some(arg.clone()),
            _ => args.paths.push(PathBuf::from(arg)),
        }
    }

    Ok(args)
}

fn build_matcher(args: &Args, pattern: &str) -> Result<RegexMatcher, String> {
    if args.fixed_strings {
        return RegexMatcherBuilder::new()
            .case_insensitive(args.ignore_case)
            .build_literals(&[pattern])
            .map_err(|e| e.to_string());
    }
    let wrapped = if args.line_regexp {
        format!("^(?:{pattern})$")
    } else if args.word_regexp {
        format!(r"\b(?:{pattern})\b")
    } else {
        pattern.to_string()
    };
    RegexMatcherBuilder::new()
        .case_insensitive(args.ignore_case)
        .build(&wrapped)
        .map_err(|e| e.to_string())
}

/// One search target: either a real path (for filename prefixes and
/// recursive traversal) or stdin (`-`, or implied when no paths given).
enum Target {
    Stdin,
    File(PathBuf),
}

fn collect_targets(args: &Args) -> Vec<Target> {
    if args.paths.is_empty() {
        return vec![Target::Stdin];
    }
    let mut targets = Vec::new();
    for path in &args.paths {
        if path.as_os_str() == "-" {
            targets.push(Target::Stdin);
        } else if path.is_dir() {
            if args.recursive {
                collect_dir_recursive(path, &mut targets);
            } else {
                eprintln!("grep: {}: Is a directory", path.display());
            }
        } else {
            targets.push(Target::File(path.clone()));
        }
    }
    targets
}

fn collect_dir_recursive(dir: &Path, out: &mut Vec<Target>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect_dir_recursive(&path, out);
        } else {
            out.push(Target::File(path));
        }
    }
}

/// Extracts just the matched substrings from a line, for `-o`. A line can
/// contain more than one match (e.g. pattern `o` against `foo bar`), so
/// this collects all non-overlapping matches rather than just the first.
fn only_matching_pieces(matcher: &RegexMatcher, line: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let _ = matcher.find_iter(line.as_bytes(), |m| {
        pieces.push(line[m.start()..m.end()].to_string());
        true
    });
    pieces
}

fn search_target(
    matcher: &RegexMatcher,
    args: &Args,
    target: &Target,
    print_filename: bool,
    match_count: &mut u64,
) -> io::Result<bool> {
    let mut searcher = SearcherBuilder::new();
    searcher.line_number(true).invert_match(args.invert);
    let mut searcher = searcher.build();

    let label: String = match target {
        Target::Stdin => "(standard input)".to_string(),
        Target::File(p) => p.display().to_string(),
    };

    let mut found_any = false;
    let quiet = args.files_with_matches || args.files_without_match || args.count;
    let sink = UTF8(|line_number, line: &str| {
        found_any = true;
        *match_count += 1;
        if quiet {
            // -l/-L only need to know whether a match exists; -c only
            // needs the count. Stop scanning the rest of the file only
            // for -l (the other two still want an exact match count /
            // don't need early exit but also aren't harmed by continuing).
            return Ok(!args.files_with_matches);
        }
        if args.only_matching {
            for piece in only_matching_pieces(matcher, line) {
                if print_filename {
                    println!("{label}:{piece}");
                } else {
                    println!("{piece}");
                }
            }
            return Ok(true);
        }
        let prefix = match (print_filename, args.line_number) {
            (true, true) => format!("{label}:{line_number}:"),
            (true, false) => format!("{label}:"),
            (false, true) => format!("{line_number}:"),
            (false, false) => String::new(),
        };
        print!("{prefix}{line}");
        if !line.ends_with('\n') {
            println!();
        }
        Ok(true)
    });

    let result = match target {
        Target::Stdin => searcher.search_reader(matcher, io::stdin().lock(), sink),
        Target::File(path) => {
            let file = File::open(path)?;
            searcher.search_reader(matcher, BufReader::new(file), sink)
        }
    };
    result?;
    Ok(found_any)
}

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let parsed = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("grep: {e}");
            return 2;
        }
    };
    let Some(pattern) = parsed.pattern.clone() else {
        eprintln!("grep: no pattern given");
        return 2;
    };
    let matcher = match build_matcher(&parsed, &pattern) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("grep: {e}");
            return 2;
        }
    };

    let targets = collect_targets(&parsed);
    let print_filename = parsed
        .with_filename
        .unwrap_or(targets.len() > 1 || parsed.recursive);

    let mut any_match = false;
    let mut had_error = false;
    for target in &targets {
        let mut count = 0u64;
        match search_target(&matcher, &parsed, target, print_filename, &mut count) {
            Ok(found) => {
                any_match |= found;
                let label = match target {
                    Target::Stdin => "(standard input)".to_string(),
                    Target::File(p) => p.display().to_string(),
                };
                let list_this_file =
                    (parsed.files_with_matches && found) || (parsed.files_without_match && !found);
                if list_this_file {
                    println!("{label}");
                } else if parsed.count {
                    if print_filename {
                        println!("{label}:{count}");
                    } else {
                        println!("{count}");
                    }
                }
            }
            Err(e) => {
                let label = match target {
                    Target::Stdin => "(standard input)".to_string(),
                    Target::File(p) => p.display().to_string(),
                };
                eprintln!("grep: {label}: {e}");
                had_error = true;
            }
        }
    }

    if had_error {
        2
    } else if any_match {
        0
    } else {
        1
    }
}
