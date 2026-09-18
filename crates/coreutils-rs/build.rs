//! Copies each vendored `uu_*` crate's `locales/*.ftl` files next to the
//! compiled binary, at `<target-dir>/locales/<utility>/`. This is the
//! layout `uucore::locale::setup_localization` looks for relative to
//! `current_exe()` in release builds (see `resolve_locales_dir_from_exe_dir`
//! in uucore's source) — without it, every uu_* crate's `--help`/`--version`
//! falls back to raw Fluent keys like `ls-about` instead of real text,
//! since uucore's own embedded-locale fallback only works when uucore is
//! built from inside the uutils/coreutils monorepo (it looks for
//! `../uu/<name>/locales` relative to its own crate directory), which
//! isn't our layout — we're pulling `uu_*` as ordinary crates.io deps.
//!
//! Two extra copies beyond the per-utility ones, both reverse-engineered
//! from uucore's source since neither is a documented public contract:
//! - `checksum_common`'s locale (shared by the `*sum` utilities) needs to
//!   land at `<target-dir>/locales/checksum_common/`, found the normal way
//!   via `get_locales_dir("checksum_common")`.
//! - uucore's own common strings (`common-usage`, `common-error`, ...) are
//!   found by `find_uucore_locales_dir`, which — regardless of how the
//!   per-utility locales dir was resolved — always walks exactly 3 parents
//!   up from it and joins `uucore/locales`. Starting from
//!   `<target-dir>/locales/<utility>`, that lands at
//!   `<target-dir>/../uucore/locales`, i.e. one level *above* the target
//!   dir, not under its own `locales/`. This is a monorepo-layout
//!   assumption baked into uucore, not something we can influence — we
//!   just place a copy where that fixed walk expects to find it.
//!
//! Only takes effect in release builds: uucore's locale lookup checks
//! `CARGO_MANIFEST_DIR/../uu/<name>/locales` (monorepo layout) in debug
//! builds instead of the exe-relative path, and that layout doesn't exist
//! here either way, so debug builds keep showing raw keys regardless.
//!
//! Fragile by nature: this depends on private uucore implementation
//! details that could shift in a future 0.12.x patch release without any
//! public API change to warn us.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

include!("src/util_list.rs");

fn copy_ftl_files(src_locales: &Path, dest: &Path) {
    if !src_locales.is_dir() {
        return;
    }
    if let Err(e) = fs::create_dir_all(dest) {
        println!("cargo:warning=creating {}: {e}", dest.display());
        return;
    }
    let Ok(entries) = fs::read_dir(src_locales) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "ftl") {
            let file_name = path.file_name().unwrap();
            if let Err(e) = fs::copy(&path, dest.join(file_name)) {
                println!("cargo:warning=copying {}: {e}", path.display());
            }
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    // OUT_DIR is target/<profile>/build/<pkg>-<hash>/out; the compiled
    // binary lives three levels up, at target/<profile>/.
    let Some(target_dir) = out_dir.ancestors().nth(3) else {
        println!(
            "cargo:warning=could not determine target dir from OUT_DIR, skipping locale bundling"
        );
        return;
    };
    let locales_root = target_dir.join("locales");

    let metadata = match cargo_metadata::MetadataCommand::new().exec() {
        Ok(m) => m,
        Err(e) => {
            println!("cargo:warning=cargo metadata failed, skipping locale bundling: {e}");
            return;
        }
    };
    let crate_locales_dir = |crate_name: &str| -> Option<PathBuf> {
        let pkg = metadata
            .packages
            .iter()
            .find(|p| p.name.as_str() == crate_name)?;
        let crate_dir = pkg.manifest_path.parent()?;
        Some(Path::new(crate_dir.as_str()).join("locales"))
    };

    for (util_name, crate_name) in UTILS {
        if let Some(src) = crate_locales_dir(crate_name) {
            copy_ftl_files(&src, &locales_root.join(util_name));
        }
    }

    if let Some(src) = crate_locales_dir("uu_checksum_common") {
        copy_ftl_files(&src, &locales_root.join("checksum_common"));
    }

    if let Some(src) = crate_locales_dir("uucore")
        && let Some(uucore_dest_root) = target_dir.parent()
    {
        copy_ftl_files(&src, &uucore_dest_root.join("uucore").join("locales"));
    }
}
