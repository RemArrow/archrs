//! `patch`, vendoring the `patch-apply` crate (a real unified-diff
//! parser/applier — GNU patch's actual matching behavior, including
//! fuzz, is intricate enough that hand-rolling it isn't worth it) for
//! the parsing/applying step, with our own small CLI layer around it
//! for path resolution and I/O.
//!
//! Scope: unified-format patches (the only format PKGBUILDs and modern
//! `diff -u` output produce) via `-i FILE` or stdin, `-p<N>` to strip
//! leading path components (GNU patch's own default is `-p0`; PKGBUILDs
//! almost always pass `-p1` explicitly for `git diff`-style `a/`/`b/`
//! prefixes). Not implemented: context/normal diff formats, fuzzy
//! matching when line numbers have drifted, `--dry-run`, and reverse
//! patches (`-R`).

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::vec::IntoIter;

use patch_apply::Patch;

fn strip_components(path: &str, strip: usize) -> PathBuf {
    let mut components: Vec<&str> = path.split('/').collect();
    if components.len() > strip {
        components.drain(..strip);
    }
    PathBuf::from(components.join("/"))
}

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let mut strip = 0usize;
    let mut input_file: Option<String> = None;
    let mut target_file: Option<String> = None;

    let mut iter = argv.iter().skip(1);
    while let Some(arg) = iter.next() {
        if let Some(n) = arg.strip_prefix("-p") {
            match n.parse() {
                Ok(n) => strip = n,
                Err(_) => {
                    eprintln!("patch: invalid -p value: {n}");
                    return 1;
                }
            }
        } else if arg == "-i" {
            input_file = iter.next().cloned();
        } else if let Some(f) = arg.strip_prefix("-i") {
            input_file = Some(f.to_string());
        } else if !arg.starts_with('-') {
            target_file = Some(arg.clone());
        }
    }

    let diff_text = match &input_file {
        Some(path) => match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("patch: {path}: {e}");
                return 1;
            }
        },
        None => {
            let mut buf = String::new();
            if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf) {
                eprintln!("patch: reading stdin: {e}");
                return 1;
            }
            buf
        }
    };

    let patches = match Patch::from_multiple(&diff_text) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("patch: parsing diff: {e}");
            return 1;
        }
    };

    // A single positional file argument overrides the diff's own
    // recorded path — matching `patch file.c < file.diff` — but only
    // when the diff itself touches exactly one file; a multi-file
    // patch always uses each entry's own recorded path.
    let single_target_override = target_file.is_some() && patches.len() == 1;

    let mut status = 0;
    for patch in patches {
        let dest: PathBuf = if single_target_override {
            PathBuf::from(target_file.as_ref().unwrap())
        } else {
            strip_components(&patch.new.path, strip)
        };

        let original = match fs::read_to_string(&dest) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("patch: {}: {e}", dest.display());
                status = 1;
                continue;
            }
        };
        let had_trailing_newline = original.ends_with('\n');

        // `patch_apply::apply` splits on `.lines()` and rejoins with
        // `Vec::join("\n")`, which always drops a trailing newline
        // regardless of whether the original file had one — a real
        // bug in that crate (0.8.3), not something wrong with how we
        // call it. Restore it ourselves for the common case (a file
        // that had one to begin with).
        let mut patched = patch_apply::apply(original, patch);
        if had_trailing_newline && !patched.ends_with('\n') {
            patched.push('\n');
        }
        if let Err(e) = fs::write(&dest, patched) {
            eprintln!("patch: writing {}: {e}", dest.display());
            status = 1;
            continue;
        }
        println!("patching file {}", dest.display());
    }

    status
}
