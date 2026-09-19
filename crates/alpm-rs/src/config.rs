use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read {0}: {1}")]
    Io(PathBuf, std::io::Error),
}

/// A single repository section from pacman.conf, e.g. [core], [extra].
#[derive(Debug, Clone, Default)]
pub struct Repo {
    pub name: String,
    pub servers: Vec<String>,
    pub includes: Vec<PathBuf>,
    pub siglevel: Option<String>,
}

/// Parsed representation of pacman.conf's [options] section plus repos.
#[derive(Debug, Clone)]
pub struct PacmanConfig {
    pub root_dir: PathBuf,
    pub db_path: PathBuf,
    pub cache_dirs: Vec<PathBuf>,
    pub log_file: PathBuf,
    pub gpg_dir: PathBuf,
    pub hold_pkg: Vec<String>,
    pub arch: String,
    pub options: BTreeMap<String, String>,
    pub repos: Vec<Repo>,
    /// Tracks whether `CacheDir` has been set explicitly yet, so the first
    /// occurrence in the config file replaces the built-in default instead
    /// of appending to it (matching pacman's own behavior).
    cache_dir_explicit: bool,
}

impl Default for PacmanConfig {
    fn default() -> Self {
        Self {
            root_dir: PathBuf::from("/"),
            db_path: PathBuf::from("/var/lib/pacman/"),
            cache_dirs: vec![PathBuf::from("/var/cache/pacman/pkg/")],
            log_file: PathBuf::from("/var/log/pacman.log"),
            gpg_dir: PathBuf::from("/etc/pacman.d/gnupg/"),
            hold_pkg: Vec::new(),
            arch: std::env::consts::ARCH.to_string(),
            options: BTreeMap::new(),
            repos: Vec::new(),
            cache_dir_explicit: false,
        }
    }
}

impl PacmanConfig {
    pub fn parse_file(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|e| ConfigError::Io(path.to_path_buf(), e))?;
        Ok(Self::parse_str(&text))
    }

    /// Parse pacman.conf syntax: `[section]` headers, `Key = Value` or bare
    /// `Key` flags, `#` comments. Does not resolve `Include=` files (sync
    /// repo server lists) — that's a separate step since it touches disk
    /// for each included path.
    pub fn parse_str(text: &str) -> Self {
        let mut cfg = PacmanConfig::default();
        let mut current_section = "options".to_string();
        let mut current_repo: Option<Repo> = None;

        for raw_line in text.lines() {
            let line = strip_comment(raw_line).trim();
            if line.is_empty() {
                continue;
            }

            if let Some(section) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                if let Some(repo) = current_repo.take() {
                    cfg.repos.push(repo);
                }
                current_section = section.to_string();
                if current_section != "options" {
                    current_repo = Some(Repo {
                        name: current_section.clone(),
                        ..Default::default()
                    });
                }
                continue;
            }

            let (key, value) = match line.split_once('=') {
                Some((k, v)) => (k.trim(), Some(v.trim().to_string())),
                None => (line.trim(), None),
            };

            if current_section == "options" {
                apply_option(&mut cfg, key, value);
            } else if let Some(repo) = current_repo.as_mut() {
                apply_repo_option(repo, key, value);
            }
        }

        if let Some(repo) = current_repo.take() {
            cfg.repos.push(repo);
        }

        cfg
    }
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(idx) => &line[..idx],
        None => line,
    }
}

fn apply_option(cfg: &mut PacmanConfig, key: &str, value: Option<String>) {
    match (key, value) {
        ("RootDir", Some(v)) => cfg.root_dir = PathBuf::from(v),
        ("DBPath", Some(v)) => cfg.db_path = PathBuf::from(v),
        ("CacheDir", Some(v)) => {
            if !cfg.cache_dir_explicit {
                cfg.cache_dirs.clear();
                cfg.cache_dir_explicit = true;
            }
            cfg.cache_dirs.push(PathBuf::from(v));
        }
        ("LogFile", Some(v)) => cfg.log_file = PathBuf::from(v),
        ("GPGDir", Some(v)) => cfg.gpg_dir = PathBuf::from(v),
        ("HoldPkg", Some(v)) => cfg.hold_pkg = v.split_whitespace().map(String::from).collect(),
        ("Architecture", Some(v)) if v != "auto" => cfg.arch = v,
        (key, Some(v)) => {
            cfg.options.insert(key.to_string(), v);
        }
        (key, None) => {
            cfg.options.insert(key.to_string(), String::new());
        }
    }
}

fn apply_repo_option(repo: &mut Repo, key: &str, value: Option<String>) {
    match (key, value) {
        ("Server", Some(v)) => repo.servers.push(v),
        ("Include", Some(v)) => repo.includes.push(PathBuf::from(v)),
        ("SigLevel", Some(v)) => repo.siglevel = Some(v),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_options_and_repos() {
        let text = r#"
[options]
CacheDir = /var/cache/pacman/pkg/
HoldPkg  = pacman glibc

[core]
Include = /etc/pacman.d/mirrorlist

[custom]
SigLevel = Optional TrustAll
Server = https://example.com/$repo/os/$arch
"#;
        let cfg = PacmanConfig::parse_str(text);
        assert_eq!(
            cfg.cache_dirs,
            vec![PathBuf::from("/var/cache/pacman/pkg/")]
        );
        assert_eq!(cfg.hold_pkg, vec!["pacman", "glibc"]);
        assert_eq!(cfg.repos.len(), 2);
        assert_eq!(cfg.repos[0].name, "core");
        assert_eq!(
            cfg.repos[0].includes,
            vec![PathBuf::from("/etc/pacman.d/mirrorlist")]
        );
        assert_eq!(cfg.repos[1].name, "custom");
        assert_eq!(cfg.repos[1].siglevel.as_deref(), Some("Optional TrustAll"));
        assert_eq!(
            cfg.repos[1].servers,
            vec!["https://example.com/$repo/os/$arch"]
        );
    }
}
