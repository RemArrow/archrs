//! `which`, vendoring the `which` crate (a real, cross-platform PATH
//! lookup implementation — not worth re-deriving `PATH` search/
//! executable-bit semantics by hand) rather than reimplementing it.
//!
//! Scope: plain lookup and `-a` (show every match in `PATH`, not just
//! the first). Not implemented: `-s` (silent) and shell-alias/function
//! awareness (`which` can't see a calling shell's aliases/functions
//! from a separate process anyway — real GNU `which` documents this
//! same limitation).

use std::ffi::OsString;
use std::vec::IntoIter;

pub fn run(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let mut show_all = false;
    let mut names = Vec::new();

    for arg in argv.iter().skip(1) {
        if arg == "-a" {
            show_all = true;
        } else {
            names.push(arg.as_str());
        }
    }

    if names.is_empty() {
        eprintln!("which: no command given");
        return 1;
    }

    let mut all_found = true;
    for name in names {
        if show_all {
            match which::which_all(name) {
                Ok(paths) => {
                    let mut found = false;
                    for path in paths {
                        println!("{}", path.display());
                        found = true;
                    }
                    all_found &= found;
                }
                Err(_) => all_found = false,
            }
        } else {
            match which::which(name) {
                Ok(path) => println!("{}", path.display()),
                Err(_) => all_found = false,
            }
        }
    }

    i32::from(!all_found)
}
