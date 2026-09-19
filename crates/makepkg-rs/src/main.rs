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
//! 2. Fetch each `source=()` entry: a VCS URL (`git+<url>`, or a bare
//!    `git://` some upstream PKGBUILDs use directly) clones via the
//!    real `git` binary into `$srcdir/<reponame>` (see
//!    `git_vcs_source`); anything else that's a URL downloads via
//!    `ureq` (matching how `alpm-rs` already fetches real package
//!    archives) and is verified against whichever of `b2sums`/
//!    `sha512sums`/`sha384sums`/`sha256sums`/`sha224sums`/`sha1sums`/
//!    `md5sums` the PKGBUILD defines (`alpm_rs::verify::ChecksumKind`
//!    — every real makepkg checksum variant except `cksums`, a
//!    non-cryptographic CRC different enough in kind to not fit this
//!    path; `"SKIP"` entries skip verification, same convention as
//!    real makepkg — but a remote source with *no* recognized
//!    checksum at all is a hard error, not a silent pass-through, see
//!    `prepare_sources`), then extracts recognized archive formats
//!    (`.tar`/`.tar.gz`/`.tar.zst`/`.zip`) into `src/` — unless it's
//!    named in `noextract=()`, in which case it's left as the plain
//!    downloaded file for the PKGBUILD's own `prepare()`/`build()` to
//!    handle itself.
//!    `source_$CARCH`/`*sums_$CARCH`/`depends_$CARCH` (architecture-
//!    specific arrays real `-bin` packages commonly use) are
//!    concatenated onto the plain array of the same name rather than
//!    replacing it, matching real makepkg (`combined_arch_array`).
//! 3. If the PKGBUILD defines `pkgver()`, run it (with `$srcdir` as its
//!    working directory) and use its output as the real package
//!    version — the mechanism every VCS-sourced PKGBUILD needs, since
//!    its real version (`git describe`, etc.) can't be known before
//!    checkout. See `run_pkgver`.
//! 4. Run `prepare`/`build`/`check` (whichever are defined) with `src/`
//!    as the working directory and the usual PKGBUILD env vars set —
//!    once, regardless of split packaging (see below), matching real
//!    makepkg.
//! 5. Run `package()` (or, for a split PKGBUILD — `pkgname=()` with
//!    more than one entry — each `package_<name>()` into its own
//!    `pkg-<name>/` directory, one output archive per name, with
//!    per-sub-package `pkgdesc`/`depends`/etc. overrides captured the
//!    same way real makepkg does: read back those variables' final
//!    values right after the function runs, in the same bash
//!    invocation — see `run_package_and_capture_metadata`) under
//!    fakeroot (the `pseudoroot` crate — a real Rust fakeroot via
//!    `LD_PRELOAD` library interposition) so ownership/permission
//!    calls succeed without needing real root, same as real makepkg.
//!    `prepare`/`build`/`check` run as the invoking user.
//! 6. Tar+zstd each pkgdir's contents into
//!    `<name>-<epoch:><ver>-<rel>-<arch>.pkg.tar.zst` (the `epoch:`
//!    prefix only when the PKGBUILD sets a nonzero `epoch=`, matching
//!    real pacman's own version-string format), with a real
//!    `.PKGINFO` member (`alpm_rs::package::write_pkginfo`) and a real
//!    `.INSTALL` member when `install=` names a scriptlet file
//!    (copied in verbatim — `pacman-rs`'s own install path is what
//!    actually runs it, at the right point in a real install/upgrade),
//!    correctly typing symlinks as symlinks rather than following them
//!    (`walk_all`/`write_root_owned_entry` — a real bug caught by
//!    testing against a real package that creates one, see below).
//!
//! Hardened against real AUR packages throughout, not just synthetic
//! test PKGBUILDs — see ROADMAP.md's "Hardened against real AUR
//! packages" section for the full story of each one:
//! - `tty-clock`: real `prepare()`/`build()`, local auxiliary source
//!   files, `b2sums` (which checksum verification didn't originally
//!   support at all — a real, caught-by-testing security gap, not a
//!   hypothetical one).
//! - `cbonsai`: a GitLab `.zip` source (unsupported at the time),
//!   compiling inside `package()` with no separate `build()`.
//! - `dmenu-git`: a real `git+https://` VCS source and `pkgver()`
//!   (no VCS handling existed at all before this); computed a real
//!   current version from a live `git describe`, differing from the
//!   AUR PKGBUILD's own stale placeholder, proving it wasn't just
//!   echoing a static value.
//! - `ttf-readex-pro`: a real split package (`pkgname=('a' 'b')`,
//!   two `package_<name>()` functions) that also creates a real
//!   symlink via `ln -s` — caught the walk/write bug above (a
//!   symlink was misclassified as a plain file, since checking
//!   `is_dir()`/`is_file()` *follows* symlinks; the written tar entry
//!   then had a header size of 0 [from the symlink's own `lstat`]
//!   but streamed the *target* file's full content [since opening a
//!   symlink path follows it], corrupting the archive).
//! - `papirus-icon-theme-git`: a real `install=` scriptlet reference
//!   (no `.INSTALL` packaging existed at all before this) and a real
//!   `epoch=1` (no epoch support existed either — the built package's
//!   version would have silently omitted it, `20260801.r0.g5f8b701-1`
//!   instead of the real `1:20260801.r0.g5f8b701-1`).
//! - `fzy`: a real `md5sums` entry — with only `b2sums`/`sha512sums`/
//!   `sha256sums` supported at the time, the refuse-if-unverified
//!   hardening `tty-clock`'s `b2sums` gap led to would have made this
//!   tool wrongly *refuse to build a legitimately checksummed
//!   package* (the opposite failure mode from `tty-clock`'s silent
//!   skip, but still a real gap: `md5sums` is old, but still real and
//!   in active use).
//! - `1password-cli`: real `source_x86_64`/`sha256sums_x86_64`
//!   architecture-specific arrays (no plain `source=()` at all — this
//!   tool would have seen nothing to build before this). Also
//!   confirmed, for real, exactly what's missing for full support:
//!   its `check()` calls a real `gpg --verify` against a signature
//!   genuinely bundled in the download, and both `check()` execution
//!   and the real `gpg` binary work correctly — the gap is
//!   specifically that nothing imports the `validpgpkeys` key first
//!   (a real, separate trust-bootstrapping design decision, not a
//!   quick addition — see ROADMAP.md).
//! - `brave-bin`: a real `noextract=()` entry (its own `prepare()`
//!   extracts the marked `.zip` itself) — confirmed, by reverting the
//!   fix and rebuilding, that this was a real **hard failure** without
//!   it (`extracting zip archive: i/o error: Is a directory`), not
//!   just wasted work.
//! - `downgrade`: real `optdepends=`/`backup=` entries. `alpm_rs`
//!   already fully modeled `optdepends`/`replaces`/`groups` (real
//!   `.PKGINFO` fields, just never read from a PKGBUILD by this tool
//!   at all); `backup=` was a deeper gap — no support anywhere in the
//!   pipeline, not even a `Package` field, before this. Both now flow
//!   through end to end, including real MD5-hash-based local-db
//!   `%BACKUP%` tracking matching real pacman's own format exactly.
//! - `dust-git`/`eza-git`: real Rust/`cargo`-based builds (`dust-git`
//!   built and even ran its own real integration test suite via
//!   `cargo test`), plus a real gap spotted *by reading* `eza-git`'s
//!   PKGBUILD rather than by running it (blocked by missing
//!   `libgit2`/`pandoc` system packages, an environment gap, not a
//!   tool one): `package() { depends+=("libgit2.so") ... }` — a
//!   metadata array reassigned *inside a plain, non-split
//!   `package()`*. Confirmed with a synthetic reproduction: an
//!   appended dependency was silently dropped from the built
//!   `.PKGINFO` before this was fixed by reusing the same capture
//!   mechanism split packages' `package_<name>()` already had.
//!
//! All ten built (or, for `1password-cli`, correctly got as far as
//! a real `gpg` "no public key" error — matching what real makepkg
//! itself would report without that key already trusted), installed
//! via `pacman-rs -U`, and ran/resolved correctly afterward. `.install`
//! execution itself (`pacman-rs`'s own side of this — see
//! `alpm_rs::install::run_install_scriptlet`) was verified separately,
//! in isolation, rather than against papirus' real scriptlet through a
//! live `-U`: it deliberately only runs against the real system root
//! (`/`), never a `--root DIR` test install, since a scriptlet meant
//! for the real system (`gtk-update-icon-cache`, etc.) would otherwise
//! wrongly act on the real host instead of the fake root being tested
//! against.
//!
//! Scope/known gaps: no PGP source verification (`validpgpkeys`/`.sig`
//! sources — confirmed, by testing against `1password-cli`'s real
//! `check()`, that the verification mechanism itself already works
//! via `alpm_rs::verify::gpg_verify` and the real `gpg` binary; what's
//! actually missing is importing `validpgpkeys`' key from a keyserver
//! first, a real trust-bootstrapping design decision deliberately not
//! made yet, not an oversight — see ROADMAP.md),
//! `svn+`/`hg+`/`bzr+` VCS sources (real but rarer than git), `cksums`
//! (a non-cryptographic CRC — every other real checksum variant is
//! supported, see above), and the `declare -p` output parser handles
//! the common case (quoted scalars and indexed arrays) rather than
//! being a fully shell-quoting-aware parser.

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
    "epoch",
    "pkgdesc",
    "url",
    "license",
    "depends",
    "makedepends",
    "optdepends",
    "provides",
    "conflicts",
    "replaces",
    "groups",
    "backup",
    "source",
    "b2sums",
    "sha512sums",
    "sha384sums",
    "sha256sums",
    "sha224sums",
    "sha1sums",
    "md5sums",
    "install",
    "noextract",
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

/// Real makepkg lets a PKGBUILD override several arrays per
/// architecture (`source_x86_64=`, `sha256sums_x86_64=`, etc.) —
/// common for `-bin` packages that publish a different download per
/// arch. These are read *in addition to* the base (arch-independent)
/// array of the same name, then concatenated onto it for building
/// (see `prepare_sources`) — real makepkg's own semantics, not an
/// override. A real gap caught by testing `1password-cli`, which
/// defines only `source_x86_64`/`sha256sums_x86_64` (no plain
/// `source=()` at all).
const ARCH_SUFFIXED_VARS: &[&str] = &[
    "source",
    "depends",
    "b2sums",
    "sha512sums",
    "sha384sums",
    "sha256sums",
    "sha224sums",
    "sha1sums",
    "md5sums",
];

fn read_pkgbuild(dir: &Path) -> Result<PkgBuild> {
    let carch = std::env::consts::ARCH;
    let arch_vars: Vec<String> = ARCH_SUFFIXED_VARS
        .iter()
        .map(|v| format!("{v}_{carch}"))
        .collect();
    let var_list = format!("{} {}", VARS.join(" "), arch_vars.join(" "));
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

/// Every `*sums=()` array real makepkg supports except `cksums` (a
/// non-cryptographic CRC, different enough in kind from the rest that
/// it doesn't fit this `Digest`-based path — the one real variant
/// still not implemented). Order is strongest/most-recommended first,
/// matching `alpm_rs::verify::ChecksumKind`'s own ordering, though in
/// practice a PKGBUILD almost always defines exactly one of these.
const CHECKSUM_ARRAYS: &[(&str, alpm_rs::verify::ChecksumKind)] = &[
    ("b2sums", alpm_rs::verify::ChecksumKind::Blake2b),
    ("sha512sums", alpm_rs::verify::ChecksumKind::Sha512),
    ("sha384sums", alpm_rs::verify::ChecksumKind::Sha384),
    ("sha256sums", alpm_rs::verify::ChecksumKind::Sha256),
    ("sha224sums", alpm_rs::verify::ChecksumKind::Sha224),
    ("sha1sums", alpm_rs::verify::ChecksumKind::Sha1),
    ("md5sums", alpm_rs::verify::ChecksumKind::Md5),
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
/// Real makepkg's `_$CARCH`-suffixed arrays are concatenated *onto the
/// end of* the plain array of the same name (not a replacement) —
/// this builds that combined list for one variable name, preserving
/// index alignment between `source`/`source_$CARCH` and each
/// `*sums`/`*sums_$CARCH` pair, since both are concatenated the same
/// way in the same order.
fn combined_arch_array(pkgbuild: &PkgBuild, var: &str) -> Vec<String> {
    let carch = std::env::consts::ARCH;
    let mut combined = pkgbuild.array(var).to_vec();
    combined.extend(pkgbuild.array(&format!("{var}_{carch}")).iter().cloned());
    combined
}

fn prepare_sources(startdir: &Path, srcdir: &Path, pkgbuild: &PkgBuild) -> Result<()> {
    fs::create_dir_all(srcdir)?;
    let sources = combined_arch_array(pkgbuild, "source");
    let checksum_lists: Vec<(&str, alpm_rs::verify::ChecksumKind, Vec<String>)> = CHECKSUM_ARRAYS
        .iter()
        .map(|(var, kind)| (*var, *kind, combined_arch_array(pkgbuild, var)))
        .collect();

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
        for (var, kind, values) in &checksum_lists {
            if let Some(expected) = values.get(i) {
                if expected == "SKIP" {
                    any_skip = true;
                } else {
                    checks.push((var, *kind, expected.as_str()));
                }
            }
        }
        if is_remote && checks.is_empty() && !any_skip {
            anyhow::bail!(
                "{filename}: no recognized checksum (b2sums/sha512sums/sha384sums/sha256sums/\
                 sha224sums/sha1sums/md5sums) to verify this remote source against — refusing \
                 to download and build unverified"
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

        // `noextract=()` names, by their post-`::`-rename destination
        // filename, sources real makepkg leaves as a whole downloaded
        // file rather than auto-extracting — real PKGBUILDs use this
        // when they extract it themselves in `prepare()`/`build()`
        // (their own choice of destination, format-specific flags,
        // etc.) rather than wanting the generic default. Extracting it
        // here anyway on top of that would be pure wasted work at
        // best — a real, previously undiscovered gap, caught by
        // testing against `brave-bin`, which extracts its own
        // `noextract`-marked `.zip` itself in `prepare()`.
        if pkgbuild.array("noextract").iter().any(|n| n == &filename) {
            continue;
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
    // as the invoking user, same as real makepkg. `package_<name>` is
    // the split-package equivalent of plain `package`, same treatment.
    let output = if func == "package" || func.starts_with("package_") {
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

/// The metadata fields a split package's own `package_<name>()`
/// function commonly overrides (`pkgdesc+=`, `depends=`, etc.) —
/// real makepkg's own mechanism for this is exactly what
/// `run_package_and_capture_metadata` below replicates: source the
/// PKGBUILD, run the function, then read back whatever these
/// variables ended up holding afterward. A sub-package that doesn't
/// touch a given variable simply reads back the same global value the
/// top-level PKGBUILD already set, so this needs no separate
/// "was it actually overridden" tracking.
const SPLIT_METADATA_VARS: &[&str] = &[
    "pkgdesc",
    "url",
    "license",
    "depends",
    "optdepends",
    "provides",
    "conflicts",
    "replaces",
    "groups",
    "backup",
    "install",
];

/// Runs a split package's `package_<name>()` function under fakeroot
/// (same as plain `package()`) and, in the same bash invocation,
/// reads back `SPLIT_METADATA_VARS`' final values afterward — the
/// same technique real makepkg uses to support per-sub-package
/// `pkgdesc`/`depends`/etc. overrides.
fn run_package_and_capture_metadata(
    startdir: &Path,
    srcdir: &Path,
    pkgdir: &Path,
    func: &str,
) -> Result<HashMap<String, Vec<String>>> {
    println!("makepkg-rs: running {func}()...");
    let var_list = SPLIT_METADATA_VARS.join(" ");
    let script = format!(
        "source ./PKGBUILD 2>/dev/null; cd \"$srcdir\" && {func}; __rc=$?; \
         echo '---PKGVARS---'; declare -p {var_list} 2>/dev/null; exit $__rc"
    );
    let carch = std::env::consts::ARCH;
    let output = bash_command()
        .arg("-c")
        .arg(&script)
        .current_dir(startdir)
        .env("srcdir", srcdir)
        .env("pkgdir", pkgdir)
        .env("startdir", startdir)
        .env("CARCH", carch)
        .fakeroot()
        .output()
        .with_context(|| format!("running {func}()"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (before, vars_part) = stdout.split_once("---PKGVARS---").unwrap_or((&stdout, ""));
    print!("{before}");
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        anyhow::bail!("{func}() failed");
    }
    Ok(parse_declare_p(vars_part))
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
/// separated from `write_header_forcing_root` so directories, files,
/// and symlinks can share the same "force root ownership" header
/// logic.
enum Entry {
    Dir(PathBuf),
    File(PathBuf),
    Symlink(PathBuf),
}

/// Symlinks are real and common in real packages — a real bug caught
/// by testing against `ttf-readex-pro` (a real split AUR package that
/// `ln -s`'s a shared fontconfig file into place): checking
/// `path.is_dir()`/implicitly-else-file (as this used to) *follows*
/// the symlink to classify it, misfiling it as a plain `File`. Writing
/// that "file" then read the symlink's *target* content via a
/// link-following `File::open`, while its tar header still carried
/// the symlink's own `lstat` size (0 bytes, from `symlink_metadata`)
/// — a header/content-length mismatch that corrupted the archive
/// (`tar: Skipping to next header` on extraction; confirmed this was
/// the actual cause by reproducing without the fix). `symlink_metadata`
/// (`lstat`, not `stat`) must be checked *before* `is_dir()`/`is_file()`
/// to classify correctly.
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
            let meta = fs::symlink_metadata(&path)?;
            if meta.is_symlink() {
                out.push(Entry::Symlink(path));
            } else if meta.is_dir() {
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
    let path = match entry {
        Entry::Dir(p) | Entry::File(p) | Entry::Symlink(p) => p,
    };
    let rel = path.strip_prefix(pkgdir)?;
    let metadata = fs::symlink_metadata(path)?;

    let mut header = tar::Header::new_gnu();
    header.set_metadata(&metadata);
    header.set_uid(0);
    header.set_gid(0);
    header.set_username("root").ok();
    header.set_groupname("root").ok();

    match entry {
        Entry::Dir(_) => {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_cksum();
            builder.append_data(&mut header, rel, std::io::empty())?;
        }
        Entry::File(_) => {
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            let mut file = File::open(path)?;
            builder.append_data(&mut header, rel, &mut file)?;
        }
        Entry::Symlink(_) => {
            let target = fs::read_link(path)?;
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            builder.append_link(&mut header, rel, &target)?;
        }
    }
    Ok(())
}

fn package_archive(
    pkgdir: &Path,
    pkg: &Package,
    out_path: &Path,
    install_file: Option<&Path>,
) -> Result<u64> {
    let file = File::create(out_path)?;
    let encoder = zstd::Encoder::new(file, 0)?.auto_finish();
    let mut builder = tar::Builder::new(encoder);

    let entries = walk_all(pkgdir)?;
    let total_size: u64 = entries
        .iter()
        .filter_map(|e| match e {
            Entry::File(p) => p.metadata().ok().map(|m| m.len()),
            Entry::Dir(_) | Entry::Symlink(_) => None,
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

    // `install=` names a real bash scriptlet (`pre_install`/
    // `post_install`/etc. function definitions) that real makepkg
    // copies into the archive as `.INSTALL`, for `pacman -U`/`-S` to
    // run at the right point in a real install/upgrade/remove — the
    // same file, unmodified, no need to inspect its contents here.
    if let Some(path) = install_file {
        let contents = fs::read(path)
            .with_context(|| format!("reading install scriptlet {}", path.display()))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_cksum();
        builder.append_data(&mut header, ".INSTALL", contents.as_slice())?;
    }

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

/// Builds a `Package` for one output archive, falling back to the
/// top-level PKGBUILD's own global arrays for any field a split
/// package's `overrides` map didn't touch (or, for a non-split build,
/// where `overrides` is simply empty and every field falls back).
fn build_package_meta(
    name: &str,
    base: &str,
    version: &str,
    carch: &str,
    pkgbuild: &PkgBuild,
    overrides: &HashMap<String, Vec<String>>,
) -> Package {
    let field = |key: &str| -> Vec<String> {
        overrides
            .get(key)
            .cloned()
            .unwrap_or_else(|| combined_arch_array(pkgbuild, key))
    };
    let scalar_field = |key: &str| -> Option<String> {
        overrides
            .get(key)
            .and_then(|v| v.first().cloned())
            .or_else(|| pkgbuild.scalar(key).map(str::to_string))
    };
    Package {
        name: name.to_string(),
        version: version.to_string(),
        base: Some(base.to_string()),
        description: scalar_field("pkgdesc"),
        url: scalar_field("url"),
        arch: Some(carch.to_string()),
        licenses: field("license"),
        depends: field("depends"),
        optdepends: field("optdepends"),
        makedepends: pkgbuild.array("makedepends").to_vec(),
        provides: field("provides"),
        conflicts: field("conflicts"),
        replaces: field("replaces"),
        groups: field("groups"),
        backup: field("backup"),
        ..Package::default()
    }
}

fn run() -> Result<()> {
    let startdir = std::env::current_dir()?;
    if !startdir.join("PKGBUILD").is_file() {
        anyhow::bail!("no PKGBUILD in {}", startdir.display());
    }

    let pkgbuild = read_pkgbuild(&startdir).context("reading PKGBUILD")?;
    let pkgnames = pkgbuild.array("pkgname").to_vec();
    if pkgnames.is_empty() {
        anyhow::bail!("PKGBUILD has no pkgname");
    }
    let mut ver = pkgbuild
        .scalar("pkgver")
        .context("PKGBUILD has no pkgver")?
        .to_string();
    let rel = pkgbuild
        .scalar("pkgrel")
        .context("PKGBUILD has no pkgrel")?
        .to_string();
    let carch = std::env::consts::ARCH.to_string();
    let base = pkgbuild
        .scalar("pkgbase")
        .map(str::to_string)
        .unwrap_or_else(|| pkgnames[0].clone());

    if pkgnames.len() > 1 {
        println!(
            "makepkg-rs: building {base} {ver}-{rel} (split: {})",
            pkgnames.join(", ")
        );
    } else {
        println!("makepkg-rs: building {base} {ver}-{rel}");
    }

    let srcdir = startdir.join("src");
    // A shared scratch pkgdir for prepare/build/check, which real
    // makepkg also runs once regardless of split packaging — only
    // package_<name>() (or plain package()) gets its own pkgdir.
    let shared_pkgdir = startdir.join("pkg");
    fs::remove_dir_all(&shared_pkgdir).ok();
    fs::create_dir_all(&shared_pkgdir)?;

    prepare_sources(&startdir, &srcdir, &pkgbuild)?;
    if let Some(computed) = run_pkgver(&startdir, &srcdir, &pkgbuild)? {
        if computed != ver {
            println!("makepkg-rs: pkgver() changed version: {ver} -> {computed}");
        }
        ver = computed;
    }
    run_pkgbuild_function(&startdir, &srcdir, &shared_pkgdir, &pkgbuild, "prepare")?;
    run_pkgbuild_function(&startdir, &srcdir, &shared_pkgdir, &pkgbuild, "build")?;
    run_pkgbuild_function(&startdir, &srcdir, &shared_pkgdir, &pkgbuild, "check")?;

    // Real pacman version strings are "epoch:pkgver-pkgrel" only when
    // epoch is set and nonzero — omitted entirely otherwise (matching
    // `alpm_rs::version::vercmp`'s own `split_evr`, which already
    // treats a missing epoch as `0`, so this doesn't need to write
    // "0:" explicitly for the common case).
    let epoch = pkgbuild
        .scalar("epoch")
        .filter(|e| *e != "0" && !e.is_empty());
    let version = match epoch {
        Some(e) => format!("{e}:{ver}-{rel}"),
        None => format!("{ver}-{rel}"),
    };

    if pkgnames.len() == 1 {
        let name = &pkgnames[0];
        // Not just `run_pkgbuild_function`: a plain (non-split)
        // `package()` can still reassign metadata arrays before
        // finishing (`depends+=('foo.so')` is a real, common pattern
        // — e.g. adding a `.so` dependency only discovered at package
        // time) — a real gap caught by reading `eza-git`'s actual
        // PKGBUILD (`depends+=("libgit2.so")` inside its own
        // `package()`), confirmed with a synthetic reproduction
        // before fixing: the appended dependency was silently
        // dropped from the built `.PKGINFO` entirely. Reusing the
        // same capture mechanism split packages already needed fixes
        // it here too, not just for `package_<name>()`.
        let overrides =
            run_package_and_capture_metadata(&startdir, &srcdir, &shared_pkgdir, "package")?;
        let pkg = build_package_meta(name, &base, &version, &carch, &pkgbuild, &overrides);
        let install_file = pkgbuild.scalar("install").map(|f| startdir.join(f));
        let out_name = format!("{name}-{version}-{carch}.pkg.tar.zst");
        let out_path = startdir.join(&out_name);
        let size = package_archive(&shared_pkgdir, &pkg, &out_path, install_file.as_deref())?;
        println!("makepkg-rs: built {out_name} ({size} bytes uncompressed)");
        return Ok(());
    }

    // Split package: each pkgname needs its own package_<name>()
    // function and its own pkgdir/output archive, matching real
    // makepkg. pkgbase is required here the same way real makepkg
    // requires it whenever pkgname is an array.
    if pkgbuild.scalar("pkgbase").is_none() {
        anyhow::bail!("split PKGBUILD (multiple pkgname entries) has no pkgbase");
    }
    for name in &pkgnames {
        let func = format!("package_{name}");
        if !pkgbuild.has_fn(&func) {
            anyhow::bail!("split PKGBUILD has no {func}() for pkgname '{name}'");
        }
        let sub_pkgdir = startdir.join(format!("pkg-{name}"));
        fs::remove_dir_all(&sub_pkgdir).ok();
        fs::create_dir_all(&sub_pkgdir)?;

        let overrides = run_package_and_capture_metadata(&startdir, &srcdir, &sub_pkgdir, &func)?;
        let pkg = build_package_meta(name, &base, &version, &carch, &pkgbuild, &overrides);
        let install_file = overrides
            .get("install")
            .and_then(|v| v.first().cloned())
            .or_else(|| pkgbuild.scalar("install").map(str::to_string))
            .map(|f| startdir.join(f));
        let out_name = format!("{name}-{version}-{carch}.pkg.tar.zst");
        let out_path = startdir.join(&out_name);
        let size = package_archive(&sub_pkgdir, &pkg, &out_path, install_file.as_deref())?;
        println!("makepkg-rs: built {out_name} ({size} bytes uncompressed)");
    }
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
