//! A `makepkg`-equivalent for archrs (ROADMAP.md Phase 4): builds a real
//! pacman package (`.pkg.tar.zst`, installable by `pacman-rs -U`) from a
//! PKGBUILD, the same way real makepkg does.
//!
//! A PKGBUILD is a bash script — there's no format to "parse" in the
//! usual sense, it has to actually be sourced by a real shell to learn
//! its variables and run its `prepare`/`build`/`check`/`package`
//! functions. This is exactly what `coreutils-rs`'s vendored `bash`
//! (see its own module docs) is for: this tool prefers a sibling
//! `coreutils-rs` binary in the same `target/<profile>/` directory
//! (dogfooding the rest of this project) and falls back to the system
//! `bash` if none is found there.
//!
//! Pipeline, matching real makepkg's shape:
//! 1. Source the PKGBUILD in a throwaway bash invocation and extract its
//!    variables via `declare -p` (and its function names via
//!    `declare -F`) — see `read_pkgbuild`.
//! 2. Download each `source=()` entry that's a URL (`ureq`, matching
//!    how `alpm-rs` already fetches real package archives), verify
//!    against `sha256sums=()` (`alpm_rs::verify`, `"SKIP"` entries
//!    skip verification, same convention as real makepkg), and extract
//!    recognized archive formats into `src/`.
//! 3. Run `prepare`/`build`/`check` (whichever are defined) with `src/`
//!    as the working directory and the usual PKGBUILD env vars set.
//! 4. Run `package()` with `pkg/` as `$pkgdir`, under fakeroot (the
//!    `pseudoroot` crate — a real Rust fakeroot via `LD_PRELOAD`
//!    library interposition) so ownership/permission calls in
//!    `package()` succeed without needing real root, same as real
//!    makepkg. `prepare`/`build`/`check` run as the invoking user.
//! 5. Tar+zstd `pkg/`'s contents into `<name>-<ver>-<rel>-<arch>.pkg.tar.zst`,
//!    with a real `.PKGINFO` member (`alpm_rs::package::write_pkginfo`).
//!
//! Scope/known gaps: single-package PKGBUILDs only (no `pkgname=()`
//! split packages), no `.install` scriptlets, no PGP source
//! verification, no `noextract`/per-source `cksums` variants beyond
//! `sha256sums`, and the `declare -p` output parser handles the common
//! case (quoted scalars and indexed arrays) rather than being a fully
//! shell-quoting-aware parser.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use alpm_rs::package::{Package, write_pkginfo};
use anyhow::{Context, Result};
use pseudoroot::FakerootCommandExt;

const VARS: &[&str] = &[
    "pkgname",
    "pkgbase",
    "pkgver",
    "pkgrel",
    "pkgdesc",
    "url",
    "license",
    "depends",
    "makedepends",
    "provides",
    "conflicts",
    "source",
    "sha256sums",
];

#[derive(Debug, Default)]
struct PkgBuild {
    vars: HashMap<String, Vec<String>>,
    functions: Vec<String>,
}

impl PkgBuild {
    fn scalar(&self, key: &str) -> Option<&str> {
        self.vars
            .get(key)
            .and_then(|v| v.first())
            .map(String::as_str)
    }

    fn array(&self, key: &str) -> &[String] {
        self.vars.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    fn has_fn(&self, name: &str) -> bool {
        self.functions.iter().any(|f| f == name)
    }
}

/// Prefers a sibling `coreutils-rs` binary (built as part of this same
/// workspace) over the system `bash`, so a fully self-hosted archrs
/// build uses its own shell rather than reaching outside itself —
/// falling back to the system `bash` when that's not available (e.g.
/// running `makepkg-rs` on its own without the rest of the workspace
/// built).
fn bash_command() -> Command {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("coreutils-rs");
        if sibling.is_file() {
            let mut cmd = Command::new(&sibling);
            cmd.arg("bash");
            return cmd;
        }
    }
    Command::new("bash")
}

fn run_bash_script(dir: &Path, script: &str) -> Result<std::process::Output> {
    bash_command()
        .arg("-c")
        .arg(script)
        .current_dir(dir)
        .output()
        .context("running bash")
}

/// Un-escapes the subset of bash's `declare -p` quoting this parser
/// handles: `\"` and `\\` inside a double-quoted value. Real bash
/// output can be considerably more elaborate for exotic values (`$`,
/// backticks, control characters); this covers what ordinary PKGBUILD
/// values (URLs, checksums, plain text) actually use.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&next) = chars.peek()
        {
            out.push(next);
            chars.next();
            continue;
        }
        out.push(c);
    }
    out
}

/// Extracts every double-quoted substring in `s`, in order, unescaping
/// each — used for both scalar values (one quoted string) and array
/// literals (`([0]="a" [1]="b")`, several).
fn extract_quoted(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut value = String::new();
        let mut escaped = false;
        let mut closed = false;
        for c in chars.by_ref() {
            if escaped {
                value.push(c);
                escaped = false;
                continue;
            }
            match c {
                '\\' => escaped = true,
                '"' => {
                    closed = true;
                    break;
                }
                other => value.push(other),
            }
        }
        if closed {
            out.push(value);
        }
    }
    out
}

fn parse_declare_p(output: &str) -> HashMap<String, Vec<String>> {
    let mut vars = HashMap::new();
    for line in output.lines() {
        let Some(rest) = line.strip_prefix("declare ") else {
            continue;
        };
        // rest looks like "-- pkgname=\"hello\"" or "-a source=(...)"
        let Some(name_value) = rest.split_whitespace().nth(1).map(|_| rest) else {
            continue;
        };
        // Skip the flags token (e.g. "--", "-a", "-ax") to get to
        // "name=value".
        let Some(eq_pos) = name_value.find('=') else {
            continue;
        };
        let before_eq = &name_value[..eq_pos];
        let Some(name) = before_eq.rsplit(' ').next() else {
            continue;
        };
        let value = &name_value[eq_pos + 1..];
        let values = extract_quoted(value)
            .into_iter()
            .map(|s| unescape(&s))
            .collect();
        vars.insert(name.to_string(), values);
    }
    vars
}

fn read_pkgbuild(dir: &Path) -> Result<PkgBuild> {
    let var_list = VARS.join(" ");
    let script = format!(
        "source ./PKGBUILD 2>/dev/null; declare -p {var_list} 2>/dev/null; echo '---FUNCTIONS---'; declare -F"
    );
    let output = run_bash_script(dir, &script)?;
    if !output.status.success() {
        anyhow::bail!(
            "sourcing PKGBUILD failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (vars_part, functions_part) = stdout
        .split_once("---FUNCTIONS---")
        .unwrap_or((&stdout, ""));

    let vars = parse_declare_p(vars_part);
    let functions = functions_part
        .lines()
        .filter_map(|l| l.strip_prefix("declare -f "))
        .map(str::to_string)
        .collect();

    Ok(PkgBuild { vars, functions })
}

fn looks_like_url(s: &str) -> bool {
    s.contains("://")
}

/// Splits a `filename::url` source entry (makepkg's "rename on
/// download" syntax) into the destination filename and the URL,
/// falling back to the URL's own last path segment when no `::` prefix
/// is given.
fn source_dest_and_url(entry: &str) -> (String, String) {
    if let Some((name, url)) = entry.split_once("::") {
        (name.to_string(), url.to_string())
    } else {
        let name = entry.rsplit('/').next().unwrap_or(entry).to_string();
        (name, entry.to_string())
    }
}

fn download(url: &str, dest: &Path) -> Result<()> {
    let resp = ureq::get(url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let mut reader = resp.into_reader();
    let mut file = File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
    std::io::copy(&mut reader, &mut file).with_context(|| format!("writing {}", dest.display()))?;
    Ok(())
}

fn extract_archive(path: &Path, dest_dir: &Path) -> Result<bool> {
    let name = path.to_string_lossy();
    let file = File::open(path)?;
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(dest_dir)?;
    } else if name.ends_with(".tar.zst") {
        tar::Archive::new(zstd::Decoder::new(file)?).unpack(dest_dir)?;
    } else if name.ends_with(".tar") {
        tar::Archive::new(file).unpack(dest_dir)?;
    } else {
        return Ok(false);
    }
    Ok(true)
}

/// Downloads (or copies, for local files) and verifies each
/// `source=()` entry, extracting recognized archive formats into
/// `srcdir` and copying everything else in as-is — matching real
/// makepkg's default (non-`noextract`) behavior.
fn prepare_sources(startdir: &Path, srcdir: &Path, pkgbuild: &PkgBuild) -> Result<()> {
    fs::create_dir_all(srcdir)?;
    let sources = pkgbuild.array("source");
    let sums = pkgbuild.array("sha256sums");

    for (i, entry) in sources.iter().enumerate() {
        let (filename, location) = source_dest_and_url(entry);
        let dest = srcdir.join(&filename);

        if looks_like_url(&location) {
            println!("makepkg-rs: downloading {filename}...");
            download(&location, &dest)?;
        } else {
            let src_path = startdir.join(&location);
            fs::copy(&src_path, &dest)
                .with_context(|| format!("copying local source {}", src_path.display()))?;
        }

        if let Some(expected) = sums.get(i)
            && expected != "SKIP"
        {
            let ok = alpm_rs::verify::verify_checksum(&dest, expected)
                .with_context(|| format!("checksumming {filename}"))?;
            if !ok {
                anyhow::bail!("sha256 mismatch for {filename}");
            }
            println!("makepkg-rs: {filename} sha256 OK");
        }

        if extract_archive(&dest, srcdir)? {
            println!("makepkg-rs: extracted {filename}");
        }
    }
    Ok(())
}

fn run_pkgbuild_function(
    startdir: &Path,
    srcdir: &Path,
    pkgdir: &Path,
    pkgbuild: &PkgBuild,
    func: &str,
) -> Result<()> {
    if !pkgbuild.has_fn(func) {
        return Ok(());
    }
    println!("makepkg-rs: running {func}()...");
    let script = format!("source ./PKGBUILD 2>/dev/null; cd \"$srcdir\" && {func}");
    let carch = std::env::consts::ARCH;
    let mut cmd = bash_command();
    cmd.arg("-c")
        .arg(&script)
        .current_dir(startdir)
        .env("srcdir", srcdir)
        .env("pkgdir", pkgdir)
        .env("startdir", startdir)
        .env("CARCH", carch);

    // Real makepkg runs only the packaging step under fakeroot, so
    // ownership/permission calls in package() (chown root:root, etc.)
    // succeed without actually needing root — prepare/build/check run
    // as the invoking user, same as real makepkg.
    let output = if func == "package" {
        cmd.fakeroot().output()
    } else {
        cmd.output()
    }
    .with_context(|| format!("running {func}()"))?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        anyhow::bail!("{func}() failed");
    }
    Ok(())
}

/// Every entry `package_archive` writes for `pkgdir`'s own contents,
/// separated from `write_header_forcing_root` so both directories and
/// files can share the same "force root ownership" header logic.
enum Entry {
    Dir(PathBuf),
    File(PathBuf),
}

fn walk_all(dir: &Path) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let mut children: Vec<PathBuf> = fs::read_dir(&current)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        children.sort();
        for path in children {
            if path.is_dir() {
                out.push(Entry::Dir(path.clone()));
                stack.push(path);
            } else {
                out.push(Entry::File(path));
            }
        }
    }
    Ok(out)
}

/// Writes one tar entry with its uid/gid forced to 0/0 (root), keeping
/// everything else (mode, mtime, size) from the real filesystem.
///
/// This is the packaging-time equivalent of running under fakeroot:
/// `package()` itself already runs under a real fakeroot session (see
/// `run_pkgbuild_function`) so ownership calls *inside* it succeed, but
/// that session only fakes what that one subprocess's own syscalls see
/// — it can't retroactively change what *this* process (building the
/// tar afterward, in-process, no subprocess involved) reads back from
/// the real filesystem, which is still genuinely owned by the invoking
/// user. Real makepkg avoids this by building the tar itself from
/// *inside* the same fakeroot session. We don't have that option
/// (there's no subprocess here to wrap), so instead this forces root
/// ownership unconditionally on every entry — correct for the
/// overwhelming majority of real packages, which rely on fakeroot's
/// ambient default (root) rather than `package()` explicitly chowning
/// specific files to some other uid/gid. A PKGBUILD that deliberately
/// assigns non-root ownership to a subset of files would be recorded
/// as root anyway — a known, documented gap.
fn write_root_owned_entry<W: Write>(
    builder: &mut tar::Builder<W>,
    entry: &Entry,
    pkgdir: &Path,
) -> Result<()> {
    let (path, is_dir) = match entry {
        Entry::Dir(p) => (p, true),
        Entry::File(p) => (p, false),
    };
    let rel = path.strip_prefix(pkgdir)?;
    let metadata = fs::symlink_metadata(path)?;

    let mut header = tar::Header::new_gnu();
    header.set_metadata(&metadata);
    header.set_uid(0);
    header.set_gid(0);
    header.set_username("root").ok();
    header.set_groupname("root").ok();

    if is_dir {
        header.set_entry_type(tar::EntryType::Directory);
        header.set_size(0);
        header.set_cksum();
        builder.append_data(&mut header, rel, std::io::empty())?;
    } else {
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        let mut file = File::open(path)?;
        builder.append_data(&mut header, rel, &mut file)?;
    }
    Ok(())
}

fn package_archive(pkgdir: &Path, pkg: &Package, out_path: &Path) -> Result<u64> {
    let file = File::create(out_path)?;
    let encoder = zstd::Encoder::new(file, 0)?.auto_finish();
    let mut builder = tar::Builder::new(encoder);

    let entries = walk_all(pkgdir)?;
    let total_size: u64 = entries
        .iter()
        .filter_map(|e| match e {
            Entry::File(p) => p.metadata().ok().map(|m| m.len()),
            Entry::Dir(_) => None,
        })
        .sum();

    let pkginfo = write_pkginfo(pkg, "archrs <makepkg-rs@localhost>", now(), total_size);
    let mut header = tar::Header::new_gnu();
    header.set_size(pkginfo.len() as u64);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder.append_data(&mut header, ".PKGINFO", pkginfo.as_bytes())?;

    for entry in &entries {
        write_root_owned_entry(&mut builder, entry, pkgdir)?;
    }

    builder.into_inner()?.flush()?;
    Ok(total_size)
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn run() -> Result<()> {
    let startdir = std::env::current_dir()?;
    if !startdir.join("PKGBUILD").is_file() {
        anyhow::bail!("no PKGBUILD in {}", startdir.display());
    }

    let pkgbuild = read_pkgbuild(&startdir).context("reading PKGBUILD")?;
    let name = pkgbuild
        .scalar("pkgname")
        .context("PKGBUILD has no pkgname")?
        .to_string();
    let ver = pkgbuild
        .scalar("pkgver")
        .context("PKGBUILD has no pkgver")?
        .to_string();
    let rel = pkgbuild
        .scalar("pkgrel")
        .context("PKGBUILD has no pkgrel")?
        .to_string();
    let carch = std::env::consts::ARCH.to_string();

    println!("makepkg-rs: building {name} {ver}-{rel}");

    let srcdir = startdir.join("src");
    let pkgdir = startdir.join("pkg");
    fs::remove_dir_all(&pkgdir).ok();
    fs::create_dir_all(&pkgdir)?;

    prepare_sources(&startdir, &srcdir, &pkgbuild)?;
    run_pkgbuild_function(&startdir, &srcdir, &pkgdir, &pkgbuild, "prepare")?;
    run_pkgbuild_function(&startdir, &srcdir, &pkgdir, &pkgbuild, "build")?;
    run_pkgbuild_function(&startdir, &srcdir, &pkgdir, &pkgbuild, "check")?;
    run_pkgbuild_function(&startdir, &srcdir, &pkgdir, &pkgbuild, "package")?;

    let pkg = Package {
        name: name.clone(),
        version: format!("{ver}-{rel}"),
        base: pkgbuild
            .scalar("pkgbase")
            .map(str::to_string)
            .or_else(|| Some(name.clone())),
        description: pkgbuild.scalar("pkgdesc").map(str::to_string),
        url: pkgbuild.scalar("url").map(str::to_string),
        arch: Some(carch.clone()),
        licenses: pkgbuild.array("license").to_vec(),
        depends: pkgbuild.array("depends").to_vec(),
        makedepends: pkgbuild.array("makedepends").to_vec(),
        provides: pkgbuild.array("provides").to_vec(),
        conflicts: pkgbuild.array("conflicts").to_vec(),
        ..Package::default()
    };

    let out_name = format!("{name}-{ver}-{rel}-{carch}.pkg.tar.zst");
    let out_path = startdir.join(&out_name);
    let size = package_archive(&pkgdir, &pkg, &out_path)?;

    println!("makepkg-rs: built {out_name} ({size} bytes uncompressed)");
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("makepkg-rs: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
