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
//!    how `alpm-rs` already fetches real package archives), verify it
//!    against whichever of `b2sums`/`sha512sums`/`sha256sums` the
//!    PKGBUILD defines (`alpm_rs::verify::ChecksumKind`; `"SKIP"`
//!    entries skip verification, same convention as real makepkg —
//!    but a remote source with *no* recognized checksum at all is a
//!    hard error, not a silent pass-through, see `prepare_sources`),
//!    and extract recognized archive formats
//!    (`.tar`/`.tar.gz`/`.tar.zst`/`.zip`) into `src/`.
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
//! Hardened against two real AUR packages, not just synthetic test
//! PKGBUILDs (see ROADMAP.md's "Hardened against real AUR packages"
//! section for the full story): `tty-clock` (real `prepare()`/
//! `build()`, local auxiliary source files, `b2sums` — which the
//! checksum handling above didn't originally support at all, a real
//! caught-by-testing security gap, not a hypothetical one) and
//! `cbonsai` (a GitLab `.zip` source, compiling inside `package()`
//! with no separate `build()`). Both built, installed via
//! `pacman-rs -U`, and ran successfully.
//!
//! Scope/known gaps: single-package PKGBUILDs only (no `pkgname=()`
//! split packages), no `.install` scriptlets, no PGP source
//! verification, no `noextract`, `md5sums`/`sha1sums`/`sha224sums`/
//! `sha384sums`/`cksums` (real but rare checksum variants — an entry
//! using only one of these is treated as unverifiable, see above,
//! same as having none at all), and the `declare -p` output parser
//! handles the common case (quoted scalars and indexed arrays) rather
//! than being a fully shell-quoting-aware parser.

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
    "sha512sums",
    "b2sums",
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

/// A VCS `source=()` entry's real URL (`git+https://...` → strip the
/// `git+`) plus an optional `#tag=`/`#branch=`/`#commit=` fragment —
/// makepkg's own syntax for pinning a VCS source to something other
/// than the default branch's tip.
struct VcsSource {
    clone_url: String,
    checkout: Option<(String, String)>,
}

/// Real makepkg supports several VCS prefixes (`git+`, `svn+`, `hg+`,
/// `bzr+`); only `git+` (and a bare `git://` URL, used by some
/// PKGBUILDs — e.g. suckless upstream ones — without the `+` prefix
/// since the scheme alone is already unambiguous) is implemented here,
/// since that covers the overwhelming majority of real VCS-sourced AUR
/// packages (`-git` split off from `-svn`/`-hg`/`-bzr` variants that
/// exist but are rare in comparison).
fn git_vcs_source(location: &str) -> Option<VcsSource> {
    let (url_part, fragment) = match location.split_once('#') {
        Some((u, f)) => (u, Some(f)),
        None => (location, None),
    };
    let clone_url = url_part
        .strip_prefix("git+")
        .map(str::to_string)
        .or_else(|| url_part.starts_with("git://").then(|| url_part.to_string()))?;
    let checkout = fragment
        .and_then(|f| f.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()));
    Some(VcsSource {
        clone_url,
        checkout,
    })
}

/// Local checkout directory name for a git source: makepkg names it
/// after the repo itself (the URL's last path segment, `.git` suffix
/// stripped), not the full source entry — `git+https://.../dmenu`
/// becomes `$srcdir/dmenu`, matching what these PKGBUILDs' own
/// `cd $_pkgname`/`git -C $_pkgname` calls already assume.
fn git_repo_name(clone_url: &str) -> String {
    clone_url
        .rsplit('/')
        .next()
        .unwrap_or(clone_url)
        .trim_end_matches(".git")
        .to_string()
}

fn clone_or_update_git(vcs: &VcsSource, dest: &Path) -> Result<()> {
    if dest.is_dir() {
        // Idempotent re-runs: a prior clone is already there and
        // pkgver()'s own `git describe` doesn't need it refreshed for
        // this project's purposes (no `--holdver`-equivalent to worry
        // about either way).
        return Ok(());
    }
    println!(
        "makepkg-rs: cloning {} ...",
        vcs.checkout
            .as_ref()
            .map(|(k, v)| format!("{} ({k}={v})", vcs.clone_url))
            .unwrap_or_else(|| vcs.clone_url.clone())
    );
    let status = Command::new("git")
        .arg("clone")
        .arg("--recursive")
        .arg(&vcs.clone_url)
        .arg(dest)
        .status()
        .context("running git clone")?;
    if !status.success() {
        anyhow::bail!("git clone failed for {}", vcs.clone_url);
    }
    if let Some((kind, value)) = &vcs.checkout {
        let target = match kind.as_str() {
            "tag" => format!("refs/tags/{value}"),
            "branch" => format!("origin/{value}"),
            _ => value.clone(),
        };
        let status = Command::new("git")
            .arg("-C")
            .arg(dest)
            .arg("checkout")
            .arg(target)
            .status()
            .context("running git checkout")?;
        if !status.success() {
            anyhow::bail!(
                "git checkout of {kind}={value} failed for {}",
                vcs.clone_url
            );
        }
    }
    Ok(())
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

/// `.zip` sources are real, not hypothetical: GitLab's own archive
/// URLs (`.../-/archive/vX.Y.Z/name-vX.Y.Z.zip`, the format several
/// real AUR PKGBUILDs use for GitLab-hosted upstreams) default to zip,
/// unlike GitHub's tarballs.
fn extract_archive(path: &Path, dest_dir: &Path) -> Result<bool> {
    let name = path.to_string_lossy();
    let file = File::open(path)?;
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(dest_dir)?;
    } else if name.ends_with(".tar.zst") {
        tar::Archive::new(zstd::Decoder::new(file)?).unpack(dest_dir)?;
    } else if name.ends_with(".tar") {
        tar::Archive::new(file).unpack(dest_dir)?;
    } else if name.ends_with(".zip") {
        zip::ZipArchive::new(file)
            .context("reading zip archive")?
            .extract(dest_dir)
            .context("extracting zip archive")?;
    } else {
        return Ok(false);
    }
    Ok(true)
}

/// The `*sums=()` arrays this tool knows how to check, in the order
/// real PKGBUILDs are checked against `alpm_rs::verify::ChecksumKind`
/// — `b2sums` first (BLAKE2b-512, makepkg's own modern default),
/// `sha512sums`, then `sha256sums`. Real makepkg additionally accepts
/// `md5sums`/`sha1sums`/`sha224sums`/`sha384sums`/`cksums`, which
/// aren't implemented (rare in practice); a PKGBUILD using only one of
/// those is treated the same as having no checksum at all (see below).
const CHECKSUM_ARRAYS: &[(&str, alpm_rs::verify::ChecksumKind)] = &[
    ("b2sums", alpm_rs::verify::ChecksumKind::Blake2b),
    ("sha512sums", alpm_rs::verify::ChecksumKind::Sha512),
    ("sha256sums", alpm_rs::verify::ChecksumKind::Sha256),
];

/// Downloads (or copies, for local files) and verifies each
/// `source=()` entry, extracting recognized archive formats into
/// `srcdir` and copying everything else in as-is — matching real
/// makepkg's default (non-`noextract`) behavior.
///
/// A remote source with no checksum this tool can verify (none of
/// `CHECKSUM_ARRAYS` has an entry for it, and it isn't marked `SKIP`)
/// is a hard error, not a silent pass-through: downloading and
/// building from an unverified remote file defeats the entire point
/// of `source=()` integrity checking. Real makepkg errors the same
/// way by default (`--skipinteg` opts out explicitly); this has no
/// equivalent opt-out, since nothing in this project's own pipeline
/// currently needs one.
fn prepare_sources(startdir: &Path, srcdir: &Path, pkgbuild: &PkgBuild) -> Result<()> {
    fs::create_dir_all(srcdir)?;
    let sources = pkgbuild.array("source");

    for (i, entry) in sources.iter().enumerate() {
        let (filename, location) = source_dest_and_url(entry);

        // VCS sources (`git+https://...`, or a bare `git://...` some
        // PKGBUILDs use directly) clone straight into `srcdir` as a
        // working tree, not a single file to checksum/extract — a
        // structurally different path from everything below.
        if let Some(vcs) = git_vcs_source(&location) {
            // An explicit `name::` prefix (from `source_dest_and_url`)
            // names the checkout directory directly, same as real
            // makepkg; otherwise it's derived from the repo URL.
            let dir_name = if entry.contains("::") {
                filename.clone()
            } else {
                git_repo_name(&vcs.clone_url)
            };
            let repo_dir = srcdir.join(dir_name);
            clone_or_update_git(&vcs, &repo_dir)?;
            continue;
        }

        let dest = srcdir.join(&filename);
        let is_remote = looks_like_url(&location);

        // Collect every recognized checksum entry for this source
        // before downloading anything, so an unverifiable remote
        // source is rejected up front rather than after wasting a
        // download.
        let mut checks: Vec<(&str, alpm_rs::verify::ChecksumKind, &str)> = Vec::new();
        let mut any_skip = false;
        for (var, kind) in CHECKSUM_ARRAYS {
            if let Some(expected) = pkgbuild.array(var).get(i) {
                if expected == "SKIP" {
                    any_skip = true;
                } else {
                    checks.push((var, *kind, expected.as_str()));
                }
            }
        }
        if is_remote && checks.is_empty() && !any_skip {
            anyhow::bail!(
                "{filename}: no recognized checksum (b2sums/sha512sums/sha256sums) to verify \
                 this remote source against — refusing to download and build unverified"
            );
        }

        if is_remote {
            println!("makepkg-rs: downloading {filename}...");
            download(&location, &dest)?;
        } else {
            let src_path = startdir.join(&location);
            fs::copy(&src_path, &dest)
                .with_context(|| format!("copying local source {}", src_path.display()))?;
        }

        for (var, kind, expected) in &checks {
            let ok = alpm_rs::verify::verify_source_checksum(&dest, *kind, expected)
                .with_context(|| format!("checksumming {filename}"))?;
            if !ok {
                anyhow::bail!("{var} mismatch for {filename}");
            }
            println!("makepkg-rs: {filename} {var} OK");
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

/// Runs a PKGBUILD's `pkgver()` function, if defined, and returns the
/// version it computes — real makepkg's mechanism for VCS-sourced
/// (`-git`/`-svn`/`-hg`) packages, whose real version (`git describe`,
/// an `svn info` revision, etc.) can't be known until the source is
/// actually checked out. Run with `$srcdir` as the working directory,
/// matching every real `pkgver()` seen in practice (they all either
/// `cd` into a source subdirectory themselves or use `git -C`), after
/// `prepare_sources` has fetched everything but before `prepare()`/
/// `build()` run — the same ordering real makepkg uses, since later
/// steps (and the final package filename) need the real version.
fn run_pkgver(startdir: &Path, srcdir: &Path, pkgbuild: &PkgBuild) -> Result<Option<String>> {
    if !pkgbuild.has_fn("pkgver") {
        return Ok(None);
    }
    println!("makepkg-rs: running pkgver()...");
    let script = "source ./PKGBUILD 2>/dev/null; cd \"$srcdir\" && pkgver";
    let output = bash_command()
        .arg("-c")
        .arg(script)
        .current_dir(startdir)
        .env("srcdir", srcdir)
        .env("startdir", startdir)
        .output()
        .context("running pkgver()")?;
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        anyhow::bail!("pkgver() failed");
    }
    let ver = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ver.is_empty() {
        anyhow::bail!("pkgver() produced no output");
    }
    Ok(Some(ver))
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
    let mut ver = pkgbuild
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
    if let Some(computed) = run_pkgver(&startdir, &srcdir, &pkgbuild)? {
        if computed != ver {
            println!("makepkg-rs: pkgver() changed version: {ver} -> {computed}");
        }
        ver = computed;
    }
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
