use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use thiserror::Error;

use crate::config::Repo;

/// `ureq`'s default agent has no timeout at all, so one unresponsive
/// mirror (dead DNS, firewalled, half-open TCP) hangs the whole install
/// forever instead of falling through to the next server in the list —
/// found by actually hanging against this system's real mirrorlist.
/// Bound both connect and total time so a bad mirror fails fast.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
    })
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("failed to read mirrorlist {0}: {1}")]
    ReadMirrorlist(PathBuf, io::Error),
    #[error("no servers configured for repo '{0}' (check Server=/Include= in pacman.conf)")]
    NoServers(String),
    #[error("HTTP request to {0} failed: {1}")]
    Request(String, Box<ureq::Error>),
    #[error("failed to write {0}: {1}")]
    Write(PathBuf, io::Error),
}

/// Every server URL template available to a repo: its own `Server=`
/// lines plus every `Server=` line in its `Include=` mirrorlist file(s),
/// in the order encountered.
pub fn resolve_servers(repo: &Repo) -> Result<Vec<String>, FetchError> {
    let mut servers = repo.servers.clone();
    for include_path in &repo.includes {
        let text = fs::read_to_string(include_path)
            .map_err(|e| FetchError::ReadMirrorlist(include_path.clone(), e))?;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if let Some(server) = line
                .strip_prefix("Server")
                .and_then(|s| s.trim_start().strip_prefix('='))
            {
                servers.push(server.trim().to_string());
            }
        }
    }
    Ok(servers)
}

/// Substitute `$repo`/`$arch` in a server URL template and append the
/// package filename, e.g. `https://mirror/$repo/$arch` + `core` + `x86_64`
/// + `acl-2.4.0-1-x86_64.pkg.tar.zst`.
pub fn package_url(server_template: &str, repo_name: &str, arch: &str, filename: &str) -> String {
    let base = server_template
        .replace("$repo", repo_name)
        .replace("$arch", arch);
    format!("{}/{}", base.trim_end_matches('/'), filename)
}

/// Download `url` to `dest`, trying each server in turn until one
/// succeeds. Returns the URL that worked.
pub fn download(
    servers: &[String],
    repo_name: &str,
    arch: &str,
    filename: &str,
    dest: &Path,
) -> Result<String, FetchError> {
    if servers.is_empty() {
        return Err(FetchError::NoServers(repo_name.to_string()));
    }

    // Check the destination is actually writable *before* touching the
    // network: this used to run after each mirror's HTTP fetch succeeded,
    // so a single unwritable cache dir (e.g. no --cachedir override to
    // match a sandboxed --root) silently burned a real request against
    // every mirror in the list before reporting the true, purely local,
    // cause.
    fs::File::create(dest).map_err(|e| FetchError::Write(dest.to_path_buf(), e))?;

    let mut last_err = None;
    for server in servers {
        let url = package_url(server, repo_name, arch, filename);
        match fetch_to_file(&url, dest) {
            Ok(()) => return Ok(url),
            Err(e) => {
                eprintln!("  mirror failed ({url}): {e}, trying next...");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap())
}

fn fetch_to_file(url: &str, dest: &Path) -> Result<(), FetchError> {
    let response = agent()
        .get(url)
        .call()
        .map_err(|e| FetchError::Request(url.to_string(), Box::new(e)))?;
    let mut reader = response.into_reader();
    let mut file = fs::File::create(dest).map_err(|e| FetchError::Write(dest.to_path_buf(), e))?;
    io::copy(&mut reader, &mut file).map_err(|e| FetchError::Write(dest.to_path_buf(), e))?;
    Ok(())
}
