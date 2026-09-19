//! A Rust reimplementation of pacman's query (`-Q`) and sync (`-S`)
//! operations. Real pacman combines an operation letter with modifier
//! letters in one flag, e.g. `-Qi`, `-Sp` — this parses that same style
//! rather than a generic subcommand CLI, so usage matches pacman itself.
//!
//! `-S` with no modifier performs a real install: fetch from a configured
//! mirror, verify the sha256sum and (best-effort) GPG signature, and
//! extract into `--root` (default `/`, matching pacman itself). Point
//! `--root`/`--dbpath` at a scratch directory to try it out without
//! touching the real system.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use alpm_rs::install::extract_package;
use alpm_rs::{LocalDb, PacmanConfig, SyncDb, Universe, fetch, remove, resolve, verify};
use anyhow::{Context, Result};

const DEFAULT_CONFIG: &str = "/etc/pacman.conf";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Query,
    Sync,
    Remove,
    InstallLocal,
}

struct Args {
    op: Option<Op>,
    info: bool,
    list: bool,
    search: bool,
    print: bool,
    refresh: bool,
    upgrade: bool,
    recursive: bool,
    nodeps: bool,
    root: Option<PathBuf>,
    dbpath: Option<PathBuf>,
    cachedir: Option<PathBuf>,
    targets: Vec<String>,
}

fn parse_args() -> Result<Args> {
    let mut args = Args {
        op: None,
        info: false,
        list: false,
        search: false,
        print: false,
        refresh: false,
        upgrade: false,
        recursive: false,
        nodeps: false,
        root: None,
        dbpath: None,
        cachedir: None,
        targets: Vec::new(),
    };

    let mut raw = std::env::args().skip(1).peekable();
    while let Some(arg) = raw.next() {
        if let Some(flags) = arg.strip_prefix("-Q") {
            set_op(&mut args, Op::Query)?;
            for c in flags.chars() {
                match c {
                    'i' => args.info = true,
                    'l' => args.list = true,
                    's' => args.search = true,
                    other => anyhow::bail!("unknown query modifier: -{other}"),
                }
            }
        } else if let Some(flags) = arg.strip_prefix("-S") {
            set_op(&mut args, Op::Sync)?;
            for c in flags.chars() {
                match c {
                    'i' => args.info = true,
                    'p' => args.print = true,
                    'y' => args.refresh = true,
                    'u' => args.upgrade = true,
                    other => anyhow::bail!("unknown sync modifier: -{other}"),
                }
            }
        } else if let Some(flags) = arg.strip_prefix("-R") {
            set_op(&mut args, Op::Remove)?;
            for c in flags.chars() {
                match c {
                    's' => args.recursive = true,
                    other => anyhow::bail!("unknown remove modifier: -{other}"),
                }
            }
        } else if arg.strip_prefix("-U").is_some_and(|flags| flags.is_empty()) {
            set_op(&mut args, Op::InstallLocal)?;
        } else if arg == "--nodeps" {
            args.nodeps = true;
        } else if arg == "--root" {
            args.root = Some(PathBuf::from(raw.next().context("--root requires a path")?));
        } else if arg == "--dbpath" {
            args.dbpath = Some(PathBuf::from(
                raw.next().context("--dbpath requires a path")?,
            ));
        } else if arg == "--cachedir" {
            args.cachedir = Some(PathBuf::from(
                raw.next().context("--cachedir requires a path")?,
            ));
        } else if arg.starts_with('-') {
            anyhow::bail!(
                "unsupported flag: {arg} (only -Q[ils], -S[ipyu], -R[s], -U, --nodeps, --root, --dbpath, --cachedir are implemented so far)"
            );
        } else {
            args.targets.push(arg);
        }
    }

    if args.op.is_none() {
        anyhow::bail!(
            "usage: pacman-rs -Q[ils] [target...] | -S[ipyu] [target...] | -R[s] [--nodeps] target... | -U file...\n\
             [--root DIR] [--dbpath DIR] [--cachedir DIR]\n\
             (see ROADMAP.md for what's implemented)"
        );
    }

    Ok(args)
}

fn set_op(args: &mut Args, op: Op) -> Result<()> {
    if let Some(existing) = args.op
        && existing != op
    {
        anyhow::bail!("only one operation may be specified at a time");
    }
    args.op = Some(op);
    Ok(())
}

fn main() -> ExitCode {
    // Rust ignores SIGPIPE by default, so writing to a closed pipe (e.g.
    // `pacman-rs -Q | head`) surfaces as a panic on the println! rather
    // than the quiet exit every other Unix CLI gives you. Restore the
    // default disposition to match.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pacman-rs: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let args = parse_args()?;
    let mut config = PacmanConfig::parse_file(DEFAULT_CONFIG)
        .with_context(|| format!("reading {DEFAULT_CONFIG}"))?;

    if let Some(root) = &args.root {
        config.root_dir = root.clone();
    }
    if let Some(dbpath) = &args.dbpath {
        config.db_path = dbpath.clone();
    } else if let Some(root) = &args.root {
        // Mirror pacman: an explicit --root without --dbpath relocates
        // the db under the new root too, so a sandbox install is fully
        // self-contained instead of writing into the real system's db.
        config.db_path = root.join("var/lib/pacman/");
    }
    if let Some(cachedir) = &args.cachedir {
        config.cache_dirs = vec![cachedir.clone()];
    } else if let Some(root) = &args.root {
        // Same reasoning as --dbpath above: without this, a sandboxed
        // --root install still tries to write into the real system's
        // /var/cache/pacman/pkg, which found a real bug (permission
        // denied) that looked like every mirror in the list failing.
        config.cache_dirs = vec![root.join("var/cache/pacman/pkg/")];
    }

    match args.op {
        Some(Op::Query) => run_query(&args, &config),
        Some(Op::Sync) => run_sync(&args, &config),
        Some(Op::Remove) => run_remove(&args, &config),
        Some(Op::InstallLocal) => run_install_local(&args, &config),
        None => unreachable!("parse_args requires an operation"),
    }
}

/// Runs a package's `post_install`/`post_upgrade` `.install` scriptlet
/// function (whichever applies, matching `old_version`'s presence),
/// but only when `config.root_dir` is the real live root (`/`) —
/// real pacman only ever runs these against the actual system; for
/// any other root (this project's own `--root DIR` test installs,
/// used constantly by its own test suite), a scriptlet meant for the
/// real system (`gtk-update-icon-cache`, `mkinitcpio -P`, etc.) would
/// wrongly act on the real host instead of the fake root if simply
/// run as-is — this project has no real `chroot(2)` privilege to
/// sandbox it the way pacman itself does. See
/// `alpm_rs::install::run_install_scriptlet`'s own docs for why that
/// function itself doesn't make this decision.
fn run_install_scriptlet_if_real_root(
    config: &PacmanConfig,
    pkg: &alpm_rs::package::Package,
    old_version: Option<&str>,
) -> Result<()> {
    if config.root_dir != Path::new("/") {
        return Ok(());
    }
    let pkg_dir = config
        .db_path
        .join("local")
        .join(format!("{}-{}", pkg.name, pkg.version));
    let (action, scriptlet_args): (&str, Vec<&str>) = match old_version {
        Some(old) => ("post_upgrade", vec![pkg.version.as_str(), old]),
        None => ("post_install", vec![pkg.version.as_str()]),
    };
    alpm_rs::install::run_install_scriptlet(&pkg_dir, action, &scriptlet_args)
        .with_context(|| format!("running {action} scriptlet for {}", pkg.name))
}

/// `-U`: install a local package file directly, without going through a
/// sync repo — the same extraction/local-db path `-S` uses, just fed a
/// `Package` read out of the archive's own `.PKGINFO` instead of one
/// resolved from a sync db entry.
fn run_install_local(args: &Args, config: &PacmanConfig) -> Result<()> {
    if args.targets.is_empty() {
        anyhow::bail!("no package file specified");
    }

    let local = LocalDb::open(&config.db_path);
    let installed = local.packages().unwrap_or_default();

    for target in &args.targets {
        let archive_path = Path::new(target);
        let pkg = alpm_rs::install::read_pkginfo(archive_path)
            .with_context(|| format!("reading {target}"))?;

        // Upgrading: same reasoning as -S's install_all — the local db
        // is keyed by name-version, so the old version's directory
        // would otherwise stick around after the new one is written.
        if let Some(old) = installed
            .iter()
            .find(|p| p.name == pkg.name && p.version != pkg.version)
        {
            let old_dir = config
                .db_path
                .join("local")
                .join(format!("{}-{}", old.name, old.version));
            std::fs::remove_dir_all(&old_dir).ok();
        }

        // -U installs are explicit unless upgrading a package that was
        // already installed as a dependency, which keeps its reason —
        // same as -S's upgrade path.
        let existing = installed.iter().find(|p| p.name == pkg.name);
        let reason = match existing {
            Some(existing) if existing.reason.as_deref() == Some("1") => "dependency",
            _ => "explicit",
        };
        let old_version = existing
            .filter(|p| p.version != pkg.version)
            .map(|p| p.version.clone());

        println!(
            "installing {} ({}) into {}...",
            pkg.name,
            pkg.version,
            config.root_dir.display()
        );
        let files = extract_package(
            archive_path,
            &config.root_dir,
            &config.db_path,
            &pkg,
            reason,
        )
        .with_context(|| format!("extracting {target}"))?;
        println!("  {} files installed", files.len());
        run_install_scriptlet_if_real_root(config, &pkg, old_version.as_deref())?;
    }

    println!("done.");
    Ok(())
}

fn run_query(args: &Args, config: &PacmanConfig) -> Result<()> {
    let db = LocalDb::open(&config.db_path);

    if args.search {
        let pattern = args
            .targets
            .first()
            .context("-Qs requires a search pattern")?;
        for pkg in db.packages()? {
            let matches = pkg.name.contains(pattern.as_str())
                || pkg
                    .description
                    .as_deref()
                    .is_some_and(|d| d.contains(pattern.as_str()));
            if matches {
                println!(
                    "local/{} {}\n    {}",
                    pkg.name,
                    pkg.version,
                    pkg.description.as_deref().unwrap_or("")
                );
            }
        }
        return Ok(());
    }

    if args.list {
        let name = args
            .targets
            .first()
            .context("-Ql requires a package name")?;
        let pkg = db
            .find(name)?
            .with_context(|| format!("package '{name}' was not found"))?;
        for file in db.files(&pkg)? {
            println!("{} /{}", pkg.name, file);
        }
        return Ok(());
    }

    let packages = if args.targets.is_empty() {
        db.packages()?
    } else {
        let mut found = Vec::new();
        for name in &args.targets {
            let pkg = db
                .find(name)?
                .with_context(|| format!("package '{name}' was not found"))?;
            found.push(pkg);
        }
        found
    };

    for pkg in packages {
        if args.info {
            print_info(&pkg, None);
        } else {
            println!("{} {}", pkg.name, pkg.version);
        }
    }

    Ok(())
}

fn load_syncdbs(config: &PacmanConfig) -> Vec<SyncDb> {
    let mut dbs = Vec::new();
    for repo in &config.repos {
        match SyncDb::open(&repo.name, &config.db_path) {
            Ok(db) => dbs.push(db),
            Err(e) => eprintln!("pacman-rs: warning: skipping repo '{}': {e}", repo.name),
        }
    }
    dbs
}

/// Download a fresh `<repo>.db` for every configured repo, overwriting
/// whatever is under `--dbpath`/sync. Mirrors real pacman's `-Sy`.
fn refresh_syncdbs(config: &PacmanConfig) -> Result<()> {
    let sync_dir = config.db_path.join("sync");
    std::fs::create_dir_all(&sync_dir)
        .with_context(|| format!("creating {}", sync_dir.display()))?;

    for repo in &config.repos {
        let servers = fetch::resolve_servers(repo)
            .with_context(|| format!("resolving servers for {}", repo.name))?;
        let filename = format!("{}.db", repo.name);
        let dest = sync_dir.join(&filename);
        println!("syncing {}...", repo.name);
        let used_url = fetch::download(&servers, &repo.name, &config.arch, &filename, &dest)
            .with_context(|| format!("downloading {filename}"))?;
        println!("  from {used_url}");
    }
    Ok(())
}

/// Installed packages whose sync-repo version is strictly newer — the
/// set `-Su` upgrades. Matched by exact package name only (not virtual
/// `provides`), same as real pacman's upgrade matching.
fn find_upgrade_targets<'a>(
    universe: &Universe,
    installed: &'a [alpm_rs::Package],
) -> Vec<&'a str> {
    installed
        .iter()
        .filter_map(|pkg| {
            let candidate = universe.find_by_name(&pkg.name)?;
            if alpm_rs::vercmp(&candidate.package.version, &pkg.version)
                == std::cmp::Ordering::Greater
            {
                Some(pkg.name.as_str())
            } else {
                None
            }
        })
        .collect()
}

fn run_sync(args: &Args, config: &PacmanConfig) -> Result<()> {
    if args.refresh {
        refresh_syncdbs(config)?;
    }

    if args.targets.is_empty() && !args.upgrade {
        if args.refresh {
            return Ok(());
        }
        anyhow::bail!("no targets specified");
    }

    let dbs = load_syncdbs(config);
    if dbs.is_empty() {
        anyhow::bail!(
            "no sync databases could be read from {:?}/sync",
            config.db_path
        );
    }
    let universe = Universe::from_syncdbs(&dbs);

    if args.info {
        for name in &args.targets {
            let candidate = universe
                .find_by_name(name)
                .with_context(|| format!("package '{name}' was not found in any repo"))?;
            print_info(&candidate.package, Some(&candidate.repo));
        }
        return Ok(());
    }

    let local = LocalDb::open(&config.db_path);
    let installed = local.packages().unwrap_or_default();

    let mut targets: Vec<&str> = args.targets.iter().map(String::as_str).collect();
    if args.upgrade {
        for name in find_upgrade_targets(&universe, &installed) {
            if !targets.contains(&name) {
                targets.push(name);
            }
        }
        if targets.is_empty() {
            println!("nothing to do (everything is up to date)");
            return Ok(());
        }
    }

    let resolution = resolve(&universe, &targets, &installed);

    if !resolution.missing.is_empty() {
        eprintln!("unresolved dependencies:");
        for dep in &resolution.missing {
            eprintln!("  {dep}");
        }
        anyhow::bail!("could not resolve all dependencies");
    }

    if args.print {
        for candidate in &resolution.order {
            println!(
                "{}/{} {}",
                candidate.repo, candidate.package.name, candidate.package.version
            );
        }
        return Ok(());
    }

    install_all(config, &resolution.order, &targets, &installed)
}

fn install_all(
    config: &PacmanConfig,
    order: &[alpm_rs::Candidate],
    explicit_targets: &[&str],
    installed: &[alpm_rs::Package],
) -> Result<()> {
    let cache_dir = config
        .cache_dirs
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("/var/cache/pacman/pkg/"));
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("creating cache dir {}", cache_dir.display()))?;

    for candidate in order {
        let pkg = &candidate.package;
        let repo = config
            .repos
            .iter()
            .find(|r| r.name == candidate.repo)
            .with_context(|| format!("repo '{}' missing from config", candidate.repo))?;
        let filename = pkg.filename.as_ref().with_context(|| {
            format!("package '{}' has no %FILENAME% in its sync entry", pkg.name)
        })?;

        println!("resolving {}...", pkg.name);
        let servers = fetch::resolve_servers(repo)
            .with_context(|| format!("resolving servers for {}", repo.name))?;

        let dest = cache_dir.join(filename);
        println!("downloading {filename}...");
        let used_url = fetch::download(&servers, &repo.name, &config.arch, filename, &dest)
            .with_context(|| format!("downloading {filename}"))?;
        println!("  from {used_url}");

        if let Some(expected_sha256) = &pkg.sha256sum {
            let ok = verify::verify_checksum(&dest, expected_sha256)
                .with_context(|| format!("checksumming {}", dest.display()))?;
            if !ok {
                anyhow::bail!("checksum mismatch for {filename} — refusing to install");
            }
            println!("  sha256 OK");
        } else {
            eprintln!("  warning: no sha256sum recorded for {filename}, skipping checksum check");
        }

        verify_signature(config, &servers, repo, &config.arch, filename, &dest);

        // Upgrading: the local db is keyed by name-version, so the old
        // version's directory would otherwise stick around after the new
        // one is written, leaving `-Q` to find both and effectively show
        // the package installed twice.
        if let Some(old) = installed
            .iter()
            .find(|p| p.name == pkg.name && p.version != pkg.version)
        {
            let old_dir = config
                .db_path
                .join("local")
                .join(format!("{}-{}", old.name, old.version));
            std::fs::remove_dir_all(&old_dir).ok();
        }

        // An upgrade of an already-installed package keeps its existing
        // explicit/dependency status rather than recomputing it — e.g.
        // `-Su` upgrading a dependency-installed package must not
        // silently promote it to explicit just because it's a target of
        // this particular resolution.
        let reason = match installed.iter().find(|p| p.name == pkg.name) {
            Some(existing) if existing.reason.as_deref() == Some("1") => "dependency",
            Some(_) => "explicit",
            None if explicit_targets.contains(&pkg.name.as_str()) => "explicit",
            None => "dependency",
        };
        println!(
            "installing {} ({}) into {}...",
            pkg.name,
            pkg.version,
            config.root_dir.display()
        );
        let files = extract_package(&dest, &config.root_dir, &config.db_path, pkg, reason)
            .with_context(|| format!("extracting {filename}"))?;
        println!("  {} files installed", files.len());
    }

    println!("done.");
    Ok(())
}

fn verify_signature(
    config: &PacmanConfig,
    servers: &[String],
    repo: &alpm_rs::config::Repo,
    arch: &str,
    filename: &str,
    pkg_path: &Path,
) {
    let sig_filename = format!("{filename}.sig");
    let sig_dest = pkg_path.with_file_name(&sig_filename);
    match fetch::download(servers, &repo.name, arch, &sig_filename, &sig_dest) {
        Ok(_) => match verify::gpg_verify(&sig_dest, pkg_path, &config.gpg_dir) {
            Ok(true) => println!("  gpg signature OK"),
            Ok(false) => eprintln!("  warning: gpg signature verification FAILED for {filename}"),
            Err(e) => eprintln!("  warning: could not run gpg verification: {e}"),
        },
        Err(e) => eprintln!("  warning: could not fetch signature for {filename}: {e}"),
    }
}

fn run_remove(args: &Args, config: &PacmanConfig) -> Result<()> {
    if args.targets.is_empty() {
        anyhow::bail!("no targets specified");
    }

    let local = LocalDb::open(&config.db_path);
    let installed = local.packages()?;

    // Validate the explicit targets exist before expanding anything.
    for name in &args.targets {
        installed
            .iter()
            .find(|p| &p.name == name)
            .with_context(|| format!("package '{name}' is not installed"))?;
    }

    let target_names: Vec<&str> = args.targets.iter().map(String::as_str).collect();
    let removal_names: Vec<String> = if args.recursive {
        let expanded = remove::expand_recursive(&target_names, &installed);
        let extra: Vec<&str> = expanded
            .iter()
            .map(String::as_str)
            .filter(|n| !target_names.contains(n))
            .collect();
        if !extra.is_empty() {
            println!("also removing unneeded dependencies: {}", extra.join(", "));
        }
        expanded
    } else {
        args.targets.clone()
    };

    let to_remove: Vec<&alpm_rs::Package> = removal_names
        .iter()
        .map(|name| installed.iter().find(|p| &p.name == name).unwrap())
        .collect();

    if !args.nodeps {
        let also_removing: Vec<&str> = removal_names.iter().map(String::as_str).collect();
        let mut blocked = false;
        for pkg in &to_remove {
            let dependents = remove::find_reverse_dependents(pkg, &installed, &also_removing);
            if !dependents.is_empty() {
                eprintln!(
                    "error: cannot remove {} — required by: {}",
                    pkg.name,
                    dependents.join(", ")
                );
                blocked = true;
            }
        }
        if blocked {
            anyhow::bail!("removal would break dependencies (pass --nodeps to override)");
        }
    }

    for pkg in &to_remove {
        let files = local.files(pkg).unwrap_or_default();
        println!("removing {} ({})...", pkg.name, pkg.version);
        let removed = remove::remove_package(&config.root_dir, &config.db_path, pkg, &files)
            .with_context(|| format!("removing {}", pkg.name))?;
        println!("  {removed} files removed");
    }

    println!("done.");
    Ok(())
}

fn print_info(pkg: &alpm_rs::Package, repo: Option<&str>) {
    println!("Repository      : {}", repo.unwrap_or("local"));
    println!("Name            : {}", pkg.name);
    println!("Version         : {}", pkg.version);
    if let Some(desc) = &pkg.description {
        println!("Description     : {desc}");
    }
    if let Some(url) = &pkg.url {
        println!("URL             : {url}");
    }
    if !pkg.licenses.is_empty() {
        println!("Licenses        : {}", pkg.licenses.join("  "));
    }
    if !pkg.depends.is_empty() {
        println!("Depends On      : {}", pkg.depends.join("  "));
    }
    if !pkg.provides.is_empty() {
        println!("Provides        : {}", pkg.provides.join("  "));
    }
    if let Some(size) = pkg.csize {
        println!("Compressed Size : {size} B");
    }
    if let Some(size) = pkg.size {
        println!("Installed Size  : {size} B");
    }
    println!();
}
