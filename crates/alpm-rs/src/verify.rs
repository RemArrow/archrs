use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::process::Command;

use blake2::Blake2b512;
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum VerifyError {
    #[error("failed to read {0}: {1}")]
    Read(std::path::PathBuf, io::Error),
    #[error("failed to run gpg: {0}")]
    GpgSpawn(io::Error),
}

pub fn sha256_hex(path: &Path) -> Result<String, VerifyError> {
    let mut file = File::open(path).map_err(|e| VerifyError::Read(path.to_path_buf(), e))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| VerifyError::Read(path.to_path_buf(), e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn verify_checksum(path: &Path, expected_sha256: &str) -> Result<bool, VerifyError> {
    Ok(sha256_hex(path)?.eq_ignore_ascii_case(expected_sha256))
}

/// Every `*sums=()` array variant real makepkg supports, in the order
/// `makepkg-rs` prefers them when more than one is present (strongest/
/// most-recommended first). `cksums` (the old System V/POSIX `cksum`
/// CRC, not a cryptographic hash at all) is the one real variant still
/// not implemented — it's rare in practice and different enough in
/// kind (a checksum, not a hash) to not fit this same `Digest`-based
/// path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumKind {
    Blake2b,
    Sha512,
    Sha384,
    Sha256,
    Sha224,
    Sha1,
    Md5,
}

fn digest_hex<D: Digest>(path: &Path) -> Result<String, VerifyError> {
    let mut file = File::open(path).map_err(|e| VerifyError::Read(path.to_path_buf(), e))?;
    let mut hasher = D::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| VerifyError::Read(path.to_path_buf(), e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    // Hex-encoded manually rather than via `{:x}`: `GenericArray`'s
    // `LowerHex` impl needs an `Add` bound its own output-size type
    // doesn't always satisfy generically (blake2's 64-byte output hits
    // this), so a plain byte-by-byte format sidesteps it entirely.
    let digest = hasher.finalize();
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn digest_hex_for(path: &Path, kind: ChecksumKind) -> Result<String, VerifyError> {
    match kind {
        ChecksumKind::Blake2b => digest_hex::<Blake2b512>(path),
        ChecksumKind::Sha512 => digest_hex::<Sha512>(path),
        ChecksumKind::Sha384 => digest_hex::<Sha384>(path),
        ChecksumKind::Sha256 => digest_hex::<Sha256>(path),
        ChecksumKind::Sha224 => digest_hex::<Sha224>(path),
        ChecksumKind::Sha1 => digest_hex::<Sha1>(path),
        ChecksumKind::Md5 => digest_hex::<Md5>(path),
    }
}

pub fn verify_source_checksum(
    path: &Path,
    kind: ChecksumKind,
    expected_hex: &str,
) -> Result<bool, VerifyError> {
    Ok(digest_hex_for(path, kind)?.eq_ignore_ascii_case(expected_hex))
}

/// Verify an OpenPGP detached signature using the system `gpg` binary
/// against pacman's own keyring (`/etc/pacman.d/gnupg` by default),
/// rather than reimplementing OpenPGP signature verification — that's
/// security-critical code with a battle-tested implementation already
/// on every Arch/Manjaro system, so shelling out to it is the honest
/// choice here, not a shortcut.
pub fn gpg_verify(sig_path: &Path, file_path: &Path, gpg_home: &Path) -> Result<bool, VerifyError> {
    let status = Command::new("gpg")
        .arg("--homedir")
        .arg(gpg_home)
        .arg("--verify")
        .arg(sig_path)
        .arg(file_path)
        .status()
        .map_err(VerifyError::GpgSpawn)?;
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn sha256_matches_known_vector() {
        let dir = std::env::temp_dir().join(format!("archrs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hello.txt");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"hello world\n")
            .unwrap();
        // sha256sum of "hello world\n"
        let expected = "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447";
        assert_eq!(sha256_hex(&path).unwrap(), expected);
        assert!(verify_checksum(&path, expected).unwrap());
        assert!(!verify_checksum(&path, "0000").unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sha512_and_blake2b_match_known_vectors() {
        let dir = std::env::temp_dir().join(format!("archrs-test-b2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hello.txt");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"hello world\n")
            .unwrap();
        // sha512sum/b2sum of "hello world\n"
        let sha512 = "db3974a97f2407b7cae1ae637c0030687a11913274d578492558e39c16c017de84eacdc8c62fe34ee4e12b4b1428817f09b6a2760c3f8a664ceae94d2434a593";
        let blake2b = "fec91c70284c72d0d4e3684788a90de9338a5b2f47f01fedbe203cafd68708718ae5672d10eca804a8121904047d40d1d6cf11e7a76419357a9469af41f22d01";
        assert!(verify_source_checksum(&path, ChecksumKind::Sha512, sha512).unwrap());
        assert!(verify_source_checksum(&path, ChecksumKind::Blake2b, blake2b).unwrap());
        assert!(!verify_source_checksum(&path, ChecksumKind::Sha512, blake2b).unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_checksum_kinds_match_known_vectors() {
        let dir = std::env::temp_dir().join(format!("archrs-test-legacy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hello.txt");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"hello world\n")
            .unwrap();
        // md5sum/sha1sum/sha224sum/sha384sum of "hello world\n"
        let md5 = "6f5902ac237024bdd0c176cb93063dc4";
        let sha1 = "22596363b3de40b06f981fb85d82312e8c0ed511";
        let sha224 = "95041dd60ab08c0bf5636d50be85fe9790300f39eb84602858a9b430";
        let sha384 = "6b3b69ff0a404f28d75e98a066d3fc64fffd9940870cc68bece28545b9a75086b343d7a1366838083e4b8f3ca6fd3c80";
        assert!(verify_source_checksum(&path, ChecksumKind::Md5, md5).unwrap());
        assert!(verify_source_checksum(&path, ChecksumKind::Sha1, sha1).unwrap());
        assert!(verify_source_checksum(&path, ChecksumKind::Sha224, sha224).unwrap());
        assert!(verify_source_checksum(&path, ChecksumKind::Sha384, sha384).unwrap());
        assert!(!verify_source_checksum(&path, ChecksumKind::Md5, sha1).unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }
}
