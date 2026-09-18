use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use tar::Archive;
use thiserror::Error;

use crate::package::Package;

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("failed to open package archive {0}: {1}")]
    OpenArchive(PathBuf, io::Error),
    #[error("failed to read tar entry: {0}")]
    TarEntry(io::Error),
    #[error("failed to extract {0}: {1}")]
    Extract(PathBuf, io::Error),
    #[error("failed to write local db entry at {0}: {1}")]
    WriteDb(PathBuf, io::Error),
}

/// Extract a downloaded `.pkg.tar.zst` into `root`, and record it in the
/// local database under `db_path/local/<name>-<version>/` the same way
/// pacman does: filesystem entries go under `root`, while the top-level
/// `.INSTALL`/`.MTREE` members (and a freshly written `desc`/`files`) go
/// into the local db directory instead of the filesystem.
///
/// Returns the list of filesystem paths (relative to `root`) that were
/// installed.
pub fn extract_package(
    archive_path: &Path,
    root: &Path,
    db_path: &Path,
    pkg: &Package,
    reason: &str,
) -> Result<Vec<String>, InstallError> {
    let file =
        File::open(archive_path).map_err(|e| InstallError::OpenArchive(archive_path.to_path_buf(), e))?;
    let decoder = zstd::stream::read::Decoder::new(file)
        .map_err(|e| InstallError::OpenArchive(archive_path.to_path_buf(), e))?;
    let mut archive = Archive::new(decoder);

    let pkg_dir = db_path.join("local").join(format!("{}-{}", pkg.name, pkg.version));
    fs::create_dir_all(&pkg_dir).map_err(|e| InstallError::WriteDb(pkg_dir.clone(), e))?;

    let mut installed_paths = Vec::new();

    for entry in archive.entries().map_err(InstallError::TarEntry)? {
        let mut entry = entry.map_err(InstallError::TarEntry)?;
        let entry_path = entry.path().map_err(InstallError::TarEntry)?.to_path_buf();
        let entry_str = entry_path.to_string_lossy().to_string();

        match entry_str.as_str() {
            ".PKGINFO" | ".BUILDINFO" | ".CHANGELOG" => continue,
            ".INSTALL" => {
                copy_entry_to(&mut entry, &pkg_dir.join("install"))?;
                continue;
            }
            ".MTREE" => {
                copy_entry_to(&mut entry, &pkg_dir.join("mtree"))?;
                continue;
            }
            _ => {}
        }

        let is_dir = entry.header().entry_type().is_dir();
        let dest = root.join(&entry_path);

        if entry.header().entry_type() == tar::EntryType::Link {
            // `Entry::unpack` resolves a hard link's target relative to
            // the process's current directory (fine for symlinks, which
            // store their target as a plain string, but wrong for hard
            // links, which need the *already-extracted source file* to
            // exist at a real path) — found by tzdata, which hardlinks
            // most zoneinfo files together and failed to extract under a
            // non-CWD root. Resolve the link target against our root
            // ourselves instead.
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| InstallError::Extract(dest.clone(), e))?;
            }
            let link_name = entry
                .link_name()
                .map_err(|e| InstallError::Extract(dest.clone(), e))?
                .ok_or_else(|| {
                    InstallError::Extract(
                        dest.clone(),
                        io::Error::other("hard link entry has no link name"),
                    )
                })?;
            let src = root.join(&link_name);
            if dest.exists() {
                fs::remove_file(&dest).map_err(|e| InstallError::Extract(dest.clone(), e))?;
            }
            fs::hard_link(&src, &dest).map_err(|e| InstallError::Extract(dest.clone(), e))?;
        } else {
            entry
                .unpack(&dest)
                .map_err(|e| InstallError::Extract(dest.clone(), e))?;
        }

        let mut recorded = entry_str.clone();
        if is_dir && !recorded.ends_with('/') {
            recorded.push('/');
        }
        installed_paths.push(recorded);
    }

    write_local_desc(&pkg_dir, pkg, reason)?;
    write_local_files(&pkg_dir, &installed_paths)?;

    Ok(installed_paths)
}

fn copy_entry_to<R: Read>(entry: &mut tar::Entry<R>, dest: &Path) -> Result<(), InstallError> {
    let mut buf = Vec::new();
    entry
        .read_to_end(&mut buf)
        .map_err(|e| InstallError::Extract(dest.to_path_buf(), e))?;
    fs::write(dest, buf).map_err(|e| InstallError::WriteDb(dest.to_path_buf(), e))
}

fn write_local_desc(pkg_dir: &Path, pkg: &Package, reason: &str) -> Result<(), InstallError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut out = String::new();
    macro_rules! field {
        ($name:expr, $value:expr) => {
            out.push_str(&format!("%{}%\n{}\n\n", $name, $value));
        };
    }
    macro_rules! list_field {
        ($name:expr, $values:expr) => {
            if !$values.is_empty() {
                out.push_str(&format!("%{}%\n{}\n\n", $name, $values.join("\n")));
            }
        };
    }

    field!("NAME", pkg.name);
    field!("VERSION", pkg.version);
    if let Some(base) = &pkg.base {
        field!("BASE", base);
    }
    if let Some(desc) = &pkg.description {
        field!("DESC", desc);
    }
    if let Some(url) = &pkg.url {
        field!("URL", url);
    }
    if let Some(arch) = &pkg.arch {
        field!("ARCH", arch);
    }
    if let Some(bd) = pkg.build_date {
        field!("BUILDDATE", bd);
    }
    field!("INSTALLDATE", now);
    if let Some(packager) = &pkg.packager {
        field!("PACKAGER", packager);
    }
    if let Some(size) = pkg.size {
        field!("SIZE", size);
    }
    list_field!("LICENSE", pkg.licenses);
    list_field!("DEPENDS", pkg.depends);
    list_field!("OPTDEPENDS", pkg.optdepends);
    list_field!("CONFLICTS", pkg.conflicts);
    list_field!("PROVIDES", pkg.provides);
    // Real pacman only writes %REASON% for dependency installs (1) and
    // omits the field entirely for explicit ones — omission means
    // "explicit" by default, confirmed against this system's own
    // /var/lib/pacman/local/*/desc files.
    if reason == "dependency" {
        field!("REASON", "1");
    }
    field!("VALIDATION", "sha256");

    fs::write(pkg_dir.join("desc"), out).map_err(|e| InstallError::WriteDb(pkg_dir.join("desc"), e))
}

fn write_local_files(pkg_dir: &Path, paths: &[String]) -> Result<(), InstallError> {
    let mut out = String::from("%FILES%\n");
    for p in paths {
        out.push_str(p);
        out.push('\n');
    }
    let dest = pkg_dir.join("files");
    let mut f = File::create(&dest).map_err(|e| InstallError::WriteDb(dest.clone(), e))?;
    f.write_all(out.as_bytes())
        .map_err(|e| InstallError::WriteDb(dest, e))
}
