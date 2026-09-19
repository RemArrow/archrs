use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use tar::Archive;
use thiserror::Error;

use crate::package::{Package, parse_desc};

#[derive(Debug, Error)]
pub enum SyncDbError {
    #[error("failed to open sync db {0}: {1}")]
    Open(PathBuf, std::io::Error),
    #[error("failed to read tar entries in {0}: {1}")]
    Tar(PathBuf, std::io::Error),
}

/// A repo's sync database: `<db_path>/sync/<repo>.db`, a gzip'd tarball of
/// `<name>-<version>/desc` entries (pacman also emits `.files` sidecar
/// archives with per-package file lists, not read here).
pub struct SyncDb {
    pub repo: String,
    pub packages: Vec<Package>,
}

impl SyncDb {
    pub fn open(repo: &str, db_path: impl AsRef<Path>) -> Result<SyncDb, SyncDbError> {
        let path = db_path.as_ref().join("sync").join(format!("{repo}.db"));
        let file = File::open(&path).map_err(|e| SyncDbError::Open(path.clone(), e))?;
        let reader = open_reader(file).map_err(|e| SyncDbError::Open(path.clone(), e))?;
        let mut archive = Archive::new(reader);

        let mut packages = Vec::new();
        let entries = archive
            .entries()
            .map_err(|e| SyncDbError::Tar(path.clone(), e))?;

        for entry in entries {
            let mut entry = entry.map_err(|e| SyncDbError::Tar(path.clone(), e))?;
            let entry_path = entry
                .path()
                .map_err(|e| SyncDbError::Tar(path.clone(), e))?
                .to_path_buf();
            if entry_path.file_name().and_then(|n| n.to_str()) != Some("desc") {
                continue;
            }
            let mut text = String::new();
            entry
                .read_to_string(&mut text)
                .map_err(|e| SyncDbError::Tar(path.clone(), e))?;
            packages.push(parse_desc(&text));
        }

        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(SyncDb {
            repo: repo.to_string(),
            packages,
        })
    }

    pub fn find(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.name == name)
    }
}

/// A repo's `.db` file is nominally "a gzip'd tarball", but that's just
/// the historical convention — `repo-add` can emit gzip, zstd, or (found
/// by actually downloading one from a real Manjaro mirror, which serves
/// plain tar with no compression at all) uncompressed tar, all under the
/// same `.db` name. Sniff the magic bytes instead of assuming gzip.
fn open_reader(file: File) -> io::Result<Box<dyn Read>> {
    let mut reader = BufReader::new(file);
    let mut magic = [0u8; 4];
    let n = reader.read(&mut magic)?;
    let prefix = io::Cursor::new(magic[..n].to_vec()).chain(reader);

    if n >= 2 && magic[0] == 0x1f && magic[1] == 0x8b {
        Ok(Box::new(GzDecoder::new(prefix)))
    } else if n >= 4 && magic == [0x28, 0xb5, 0x2f, 0xfd] {
        Ok(Box::new(zstd::stream::read::Decoder::new(prefix)?))
    } else {
        Ok(Box::new(prefix))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn build_tar() -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let desc = b"%NAME%\ntestpkg\n\n%VERSION%\n1.0-1\n\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(desc.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "testpkg-1.0-1/desc", &desc[..])
            .unwrap();
        builder.into_inner().unwrap()
    }

    fn write_db(dir: &Path, repo: &str, bytes: &[u8]) {
        let sync_dir = dir.join("sync");
        std::fs::create_dir_all(&sync_dir).unwrap();
        std::fs::File::create(sync_dir.join(format!("{repo}.db")))
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("archrs-syncdb-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_plain_uncompressed_tar() {
        let dir = scratch_dir("plain");
        write_db(&dir, "test", &build_tar());
        let db = SyncDb::open("test", &dir).unwrap();
        assert_eq!(db.packages.len(), 1);
        assert_eq!(db.packages[0].name, "testpkg");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reads_gzip_compressed_tar() {
        use flate2::write::GzEncoder;
        let dir = scratch_dir("gzip");
        let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&build_tar()).unwrap();
        write_db(&dir, "test", &encoder.finish().unwrap());
        let db = SyncDb::open("test", &dir).unwrap();
        assert_eq!(db.packages.len(), 1);
        assert_eq!(db.packages[0].name, "testpkg");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reads_zstd_compressed_tar() {
        let dir = scratch_dir("zstd");
        let compressed = zstd::stream::encode_all(&build_tar()[..], 0).unwrap();
        write_db(&dir, "test", &compressed);
        let db = SyncDb::open("test", &dir).unwrap();
        assert_eq!(db.packages.len(), 1);
        assert_eq!(db.packages[0].name, "testpkg");
        std::fs::remove_dir_all(&dir).ok();
    }
}
