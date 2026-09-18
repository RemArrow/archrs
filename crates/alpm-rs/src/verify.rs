use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};
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
        std::fs::File::create(&path).unwrap().write_all(b"hello world\n").unwrap();
        // sha256sum of "hello world\n"
        let expected = "a948904f2f0f479b8f8197694b30184b0d2ed1c1cd2a1ec0fb85d299a192a447";
        assert_eq!(sha256_hex(&path).unwrap(), expected);
        assert!(verify_checksum(&path, expected).unwrap());
        assert!(!verify_checksum(&path, "0000").unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }
}
