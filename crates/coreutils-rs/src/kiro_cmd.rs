//! `kiro`/`nano` — a real terminal text editor via the `kiro-editor`
//! crate (rhysd's Rust port of the "kilo" text-editor tutorial: real
//! raw-terminal-mode editing, not something worth hand-rolling), the
//! last real gap from Phase 5's original "no interactive/curses tools"
//! exclusion — closed now because a "usable" system genuinely needs to
//! be able to edit a config file interactively, not just `cat`/`sed`
//! it.
//!
//! **Deliberately not aliased as `vi`/`vim`/`editor`**: `kiro-editor`'s
//! own interaction model is Ctrl-key-driven with no mode switching at
//! all (`Ctrl-S` save, `Ctrl-Q` quit, arrow keys to move, type to
//! insert) — genuinely closer to real `nano` than to real `vi`'s modal
//! editing, and aliasing it as `vi` would actively mislead anyone who
//! knows real `vi`'s keybindings into expecting modal behavior this
//! doesn't have. Registered under its own real name (`kiro`) and as
//! `nano`, not as a stand-in for `vi`.
//!
//! Not implemented: real `vi`/`vim` at all (a genuine, different, much
//! larger tool — modal editing, its own scripting language — not
//! something this crate provides even under another name), syntax
//! highlighting beyond what `kiro-editor` already does out of the box,
//! real `nano`'s own specific keybindings/config file format
//! (`kiro-editor` has its own, different Ctrl-key mapping).

use std::ffi::OsString;
use std::io;
use std::vec::IntoIter;

pub fn run(args: IntoIter<OsString>) -> i32 {
    let files: Vec<String> = args
        .skip(1) // argv[0]: our own utility name
        .map(|a| a.to_string_lossy().into_owned())
        .collect();

    let input = match kiro_editor::StdinRawMode::new() {
        Ok(i) => i.input_keys(),
        Err(e) => {
            eprintln!("kiro: could not set up terminal input: {e}");
            return 1;
        }
    };

    let editor = kiro_editor::Editor::open(input, io::stdout(), None, &files)
        .and_then(|mut editor| editor.edit());

    match editor {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("kiro: {e}");
            1
        }
    }
}
