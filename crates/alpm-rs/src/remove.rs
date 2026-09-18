use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;

use thiserror::Error;

use crate::depend::{parse_provide, Depend};
use crate::package::Package;

#[derive(Debug, Error)]
pub enum RemoveError {
    #[error("failed to remove local db entry {0}: {1}")]
    RemoveDbEntry(std::path::PathBuf, io::Error),
}

/// Names of other installed packages that declare a dependency on
/// `target` (by name or by one of `target`'s `%PROVIDES%`), excluding
/// anything in `also_removing` — real pacman refuses to remove a package
/// something else still needs, unless the whole blocking set is being
/// removed together.
pub fn find_reverse_dependents(
    target: &Package,
    installed: &[Package],
    also_removing: &[&str],
) -> Vec<String> {
    let mut provided_names: Vec<&str> = vec![target.name.as_str()];
    for p in &target.provides {
        provided_names.push(p.split('=').next().unwrap_or(p));
    }

    let mut dependents = Vec::new();
    for pkg in installed {
        if pkg.name == target.name || also_removing.contains(&pkg.name.as_str()) {
            continue;
        }
        let depends_on_target = pkg.depends.iter().any(|dep_str| {
            let dep = Depend::parse(dep_str);
            provided_names.contains(&dep.name.as_str())
        });
        if depends_on_target {
            dependents.push(pkg.name.clone());
        }
    }
    dependents
}

/// Find the installed package that satisfies `dep`, by exact name or by
/// one of its `%PROVIDES%` entries.
fn find_installed_providing<'a>(dep: &Depend, installed: &'a [Package]) -> Option<&'a Package> {
    if let Some(pkg) = installed.iter().find(|p| p.name == dep.name)
        && dep.satisfied_by(&pkg.name, Some(&pkg.version)) {
            return Some(pkg);
        }
    installed.iter().find(|pkg| {
        pkg.provides.iter().any(|provide| {
            let (pname, pver) = parse_provide(provide);
            pname == dep.name && dep.satisfied_by(&pname, pver.as_deref())
        })
    })
}

/// Expand an explicit removal set with `-Rs` semantics: any of the
/// removed packages' own dependencies that were installed *as a
/// dependency* (not explicitly) and would be left with no remaining
/// dependent once this batch is removed are pulled in too — repeated
/// until a pass finds no more orphans, since removing one dependency can
/// orphan another one further down the chain.
pub fn expand_recursive(targets: &[&str], installed: &[Package]) -> Vec<String> {
    let mut to_remove: HashSet<String> = targets.iter().map(|s| s.to_string()).collect();

    loop {
        let mut candidates = HashSet::new();
        for name in &to_remove {
            let Some(pkg) = installed.iter().find(|p| &p.name == name) else {
                continue;
            };
            for dep_str in &pkg.depends {
                let dep = Depend::parse(dep_str);
                if let Some(dep_pkg) = find_installed_providing(&dep, installed) {
                    candidates.insert(dep_pkg.name.clone());
                }
            }
        }

        let mut added = false;
        for cand in candidates {
            if to_remove.contains(&cand) {
                continue;
            }
            let Some(pkg) = installed.iter().find(|p| p.name == cand) else {
                continue;
            };
            if pkg.reason.as_deref() != Some("1") {
                continue; // only auto-remove packages installed as dependencies
            }
            let also_removing: Vec<&str> = to_remove.iter().map(String::as_str).collect();
            if find_reverse_dependents(pkg, installed, &also_removing).is_empty() {
                to_remove.insert(cand);
                added = true;
            }
        }

        if !added {
            break;
        }
    }

    to_remove.into_iter().collect()
}

/// Delete a package's files from `root` (deepest paths first, so
/// directories empty out before their own removal is attempted; a
/// directory still owned by another package is left alone rather than
/// treated as an error) and its local db entry from `db_path`. Returns
/// the number of filesystem entries actually removed.
pub fn remove_package(
    root: &Path,
    db_path: &Path,
    pkg: &Package,
    files: &[String],
) -> Result<usize, RemoveError> {
    let mut sorted: Vec<&String> = files.iter().collect();
    sorted.sort_by_key(|p| std::cmp::Reverse(p.matches('/').count()));

    let mut removed = 0;
    for rel in sorted {
        let path = root.join(rel);
        let result = if rel.ends_with('/') {
            fs::remove_dir(&path)
        } else {
            fs::remove_file(&path)
        };
        match result {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            // Directory not empty (still owned by another package) or a
            // permissions quirk — not a removal failure in either case.
            Err(_) => {}
        }
    }

    let pkg_dir = db_path.join("local").join(format!("{}-{}", pkg.name, pkg.version));
    fs::remove_dir_all(&pkg_dir).map_err(|e| RemoveError::RemoveDbEntry(pkg_dir, e))?;

    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, depends: &[&str], provides: &[&str]) -> Package {
        Package {
            name: name.to_string(),
            version: "1.0-1".to_string(),
            depends: depends.iter().map(|s| s.to_string()).collect(),
            provides: provides.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn dep_pkg(name: &str, depends: &[&str]) -> Package {
        Package {
            reason: Some("1".to_string()),
            ..pkg(name, depends, &[])
        }
    }

    #[test]
    fn finds_direct_reverse_dependents() {
        let installed = vec![pkg("a", &[], &[]), pkg("b", &["a"], &[])];
        let target = &installed[0];
        let deps = find_reverse_dependents(target, &installed, &[]);
        assert_eq!(deps, vec!["b".to_string()]);
    }

    #[test]
    fn finds_reverse_dependents_via_provides() {
        let installed = vec![pkg("bash", &[], &["sh"]), pkg("app", &["sh"], &[])];
        let target = &installed[0];
        let deps = find_reverse_dependents(target, &installed, &[]);
        assert_eq!(deps, vec!["app".to_string()]);
    }

    #[test]
    fn excludes_packages_also_being_removed() {
        let installed = vec![pkg("a", &[], &[]), pkg("b", &["a"], &[])];
        let target = &installed[0];
        let deps = find_reverse_dependents(target, &installed, &["b"]);
        assert!(deps.is_empty());
    }

    #[test]
    fn no_reverse_dependents_is_empty() {
        let installed = vec![pkg("a", &[], &[])];
        let target = &installed[0];
        assert!(find_reverse_dependents(target, &installed, &[]).is_empty());
    }

    #[test]
    fn recursive_removal_follows_orphan_chain() {
        // app (explicit) -> liba (dependency) -> libb (dependency), and
        // nothing else needs any of them: -Rs app should sweep all three.
        let installed = vec![
            pkg("app", &["liba"], &[]),
            dep_pkg("liba", &["libb"]),
            dep_pkg("libb", &[]),
        ];
        let mut removed = expand_recursive(&["app"], &installed);
        removed.sort();
        assert_eq!(removed, vec!["app", "liba", "libb"]);
    }

    #[test]
    fn recursive_removal_keeps_deps_still_needed_elsewhere() {
        let installed = vec![
            pkg("app", &["libshared"], &[]),
            pkg("other", &["libshared"], &[]),
            dep_pkg("libshared", &[]),
        ];
        let removed = expand_recursive(&["app"], &installed);
        assert_eq!(removed, vec!["app".to_string()]);
    }

    #[test]
    fn recursive_removal_does_not_sweep_explicit_deps() {
        // liba has no REASON field, i.e. it was explicitly installed even
        // though app also depends on it — -Rs must leave it alone.
        let installed = vec![pkg("app", &["liba"], &[]), pkg("liba", &[], &[])];
        let removed = expand_recursive(&["app"], &installed);
        assert_eq!(removed, vec!["app".to_string()]);
    }
}
