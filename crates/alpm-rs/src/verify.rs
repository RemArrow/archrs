use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::process::Command;

use blake2::Blake2b512;
use sha2::{Digest, Sha256, Sha512};
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

/// The `*sums=()` array variants real PKGBUILDs actually use in
/// practice for source integrity — `b2sums` (BLAKE2b-512, makepkg's
/// own recommended default since it added support) and `sha512sums`
/// alongside the already-supported `sha256sums`. `md5sums`/
/// `sha1sums`/`sha224sums`/`sha384sums`/`cksums` are real makepkg
/// variables too but rare in the wild; not implemented here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumKind {
    Sha256,
    Sha512,
    Blake2b,
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
        ChecksumKind::Sha256 => digest_hex::<Sha256>(path),
        ChecksumKind::Sha512 => digest_hex::<Sha512>(path),
        ChecksumKind::Blake2b => digest_hex::<Blake2b512>(path),
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
}
