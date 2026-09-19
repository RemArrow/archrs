use std::fs;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::package::{Package, parse_desc_file};

#[derive(Debug, Error)]
pub enum DbError {
    #[error("failed to read local db directory {0}: {1}")]
    ReadDir(PathBuf, std::io::Error),
    #[error("failed to parse desc file for entry {0}: {1}")]
    ParseEntry(PathBuf, std::io::Error),
}

/// The local package database: one directory per installed package under
/// `<db_path>/local/<name>-<version>/desc`.
pub struct LocalDb {
    local_dir: PathBuf,
}

impl LocalDb {
    pub fn open(db_path: impl AsRef<Path>) -> Self {
        Self {
            local_dir: db_path.as_ref().join("local"),
        }
    }

    /// List every installed package, parsed from its `desc` file.
    pub fn packages(&self) -> Result<Vec<Package>, DbError> {
        let entries = fs::read_dir(&self.local_dir)
            .map_err(|e| DbError::ReadDir(self.local_dir.clone(), e))?;

        let mut packages = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| DbError::ReadDir(self.local_dir.clone(), e))?;
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let desc_path = entry.path().join("desc");
            if !desc_path.exists() {
                continue;
            }
            let pkg = parse_desc_file(&desc_path)
                .map_err(|e| DbError::ParseEntry(desc_path.clone(), e))?;
            packages.push(pkg);
        }

        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    /// Find a single installed package by name.
    pub fn find(&self, name: &str) -> Result<Option<Package>, DbError> {
        Ok(self.packages()?.into_iter().find(|p| p.name == name))
    }

    /// List files owned by a package, from its `files` list.
    pub fn files(&self, pkg: &Package) -> std::io::Result<Vec<String>> {
        let dir_name = format!("{}-{}", pkg.name, pkg.version);
        let files_path = self.local_dir.join(dir_name).join("files");
        if !files_path.exists() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(files_path)?;
        Ok(text
            .lines()
            .skip_while(|l| *l != "%FILES%")
            .skip(1)
            .take_while(|l| !l.is_empty())
            .map(String::from)
            .collect())
    }
}
