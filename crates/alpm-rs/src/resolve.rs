use std::collections::{HashMap, HashSet};

use crate::depend::{Depend, parse_provide};
use crate::package::Package;
use crate::syncdb::SyncDb;

/// A package pulled in from one of the loaded sync repos.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub repo: String,
    pub package: Package,
}

/// The set of packages known from all configured sync repos, indexed by
/// name and by what they `%PROVIDES%`, so a dependency can be satisfied
/// either directly or virtually (e.g. `sh` provided by `bash`).
pub struct Universe {
    candidates: Vec<Candidate>,
    by_name: HashMap<String, Vec<usize>>,
    by_provide: HashMap<String, Vec<usize>>,
}

impl Universe {
    pub fn from_syncdbs(dbs: &[SyncDb]) -> Universe {
        let mut candidates = Vec::new();
        for db in dbs {
            for pkg in &db.packages {
                candidates.push(Candidate {
                    repo: db.repo.clone(),
                    package: pkg.clone(),
                });
            }
        }

        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_provide: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, cand) in candidates.iter().enumerate() {
            by_name
                .entry(cand.package.name.clone())
                .or_default()
                .push(idx);
            for provide in &cand.package.provides {
                let (name, _ver) = parse_provide(provide);
                by_provide.entry(name).or_default().push(idx);
            }
        }

        Universe {
            candidates,
            by_name,
            by_provide,
        }
    }

    /// Find the best candidate satisfying a dependency: prefer an exact
    /// name match over a virtual (`provides`) match, and the first repo
    /// listed wins ties (mirrors pacman's repo-priority-order behavior).
    pub fn satisfy(&self, dep: &Depend) -> Option<&Candidate> {
        if let Some(idxs) = self.by_name.get(&dep.name) {
            for &idx in idxs {
                let cand = &self.candidates[idx];
                if dep.satisfied_by(&cand.package.name, Some(&cand.package.version)) {
                    return Some(cand);
                }
            }
        }
        if let Some(idxs) = self.by_provide.get(&dep.name) {
            for &idx in idxs {
                let cand = &self.candidates[idx];
                for provide in &cand.package.provides {
                    let (pname, pver) = parse_provide(provide);
                    if pname == dep.name && dep.satisfied_by(&pname, pver.as_deref()) {
                        return Some(cand);
                    }
                }
            }
        }
        None
    }

    pub fn find_by_name(&self, name: &str) -> Option<&Candidate> {
        self.by_name
            .get(name)
            .and_then(|idxs| idxs.first())
            .map(|&idx| &self.candidates[idx])
    }
}

/// Index over already-installed packages, by name and by what they
/// `%PROVIDES%`, so a dependency can be recognized as satisfied even when
/// it's a virtual name like `ttf-font` provided by whichever font package
/// happens to be installed — not just an exact package-name match.
struct Installed<'a> {
    by_name: HashMap<&'a str, &'a Package>,
    by_provide: HashMap<String, Vec<&'a Package>>,
}

impl<'a> Installed<'a> {
    fn new(packages: &'a [Package]) -> Self {
        let by_name = packages.iter().map(|p| (p.name.as_str(), p)).collect();
        let mut by_provide: HashMap<String, Vec<&Package>> = HashMap::new();
        for pkg in packages {
            for provide in &pkg.provides {
                let (name, _ver) = parse_provide(provide);
                by_provide.entry(name).or_default().push(pkg);
            }
        }
        Installed {
            by_name,
            by_provide,
        }
    }

    fn satisfies(&self, dep: &Depend) -> bool {
        if let Some(pkg) = self.by_name.get(dep.name.as_str())
            && dep.satisfied_by(&pkg.name, Some(&pkg.version))
        {
            return true;
        }
        if let Some(pkgs) = self.by_provide.get(&dep.name) {
            for pkg in pkgs {
                for provide in &pkg.provides {
                    let (pname, pver) = parse_provide(provide);
                    if pname == dep.name && dep.satisfied_by(&pname, pver.as_deref()) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

/// The result of resolving a set of target packages against a `Universe`:
/// `order` lists packages dependencies-first (a valid install order),
/// `missing` lists dependency strings that couldn't be satisfied by
/// anything in the universe.
#[derive(Debug, Default)]
pub struct Resolution {
    pub order: Vec<Candidate>,
    pub missing: Vec<String>,
}

/// Resolve `targets` (package names) against `universe`, skipping any
/// package name already present (with a satisfying version) in
/// `already_installed` — pass an empty slice to resolve everything from
/// scratch.
pub fn resolve(universe: &Universe, targets: &[&str], already_installed: &[Package]) -> Resolution {
    let installed = Installed::new(already_installed);

    // Explicit targets are always resolved and included, even if already
    // installed (matching `pacman -S`, not `--needed`) — only transitive
    // *dependencies* get skipped when an installed version already
    // satisfies them.
    let explicit: HashSet<&str> = targets.iter().copied().collect();

    let mut visited = HashSet::new();
    let mut visiting = HashSet::new();
    let mut result = Resolution::default();

    for target in targets {
        visit(
            target,
            universe,
            &installed,
            &explicit,
            &mut visited,
            &mut visiting,
            &mut result,
        );
    }

    result
}

fn visit(
    name: &str,
    universe: &Universe,
    installed: &Installed,
    explicit: &HashSet<&str>,
    visited: &mut HashSet<String>,
    visiting: &mut HashSet<String>,
    result: &mut Resolution,
) {
    if visited.contains(name) || visiting.contains(name) {
        return;
    }
    if !explicit.contains(name) && installed.by_name.contains_key(name) {
        visited.insert(name.to_string());
        return;
    }

    let Some(candidate) = universe.find_by_name(name) else {
        result.missing.push(name.to_string());
        return;
    };

    visiting.insert(name.to_string());
    for dep_str in &candidate.package.depends {
        let dep = Depend::parse(dep_str);

        if installed.satisfies(&dep) {
            continue;
        }

        match universe.satisfy(&dep) {
            Some(dep_candidate) => {
                let dep_name = dep_candidate.package.name.clone();
                visit(
                    &dep_name, universe, installed, explicit, visited, visiting, result,
                );
            }
            None => result.missing.push(dep_str.clone()),
        }
    }
    visiting.remove(name);
    visited.insert(name.to_string());
    result.order.push(candidate.clone());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::Package;

    fn pkg(name: &str, version: &str, depends: &[&str], provides: &[&str]) -> Package {
        Package {
            name: name.to_string(),
            version: version.to_string(),
            depends: depends.iter().map(|s| s.to_string()).collect(),
            provides: provides.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    fn universe_of(pkgs: Vec<Package>) -> Universe {
        let db = SyncDb {
            repo: "test".to_string(),
            packages: pkgs,
        };
        Universe::from_syncdbs(&[db])
    }

    #[test]
    fn resolves_transitive_deps_in_order() {
        let universe = universe_of(vec![
            pkg("a", "1.0-1", &["b"], &[]),
            pkg("b", "1.0-1", &["c"], &[]),
            pkg("c", "1.0-1", &[], &[]),
        ]);
        let res = resolve(&universe, &["a"], &[]);
        assert!(res.missing.is_empty());
        let names: Vec<&str> = res.order.iter().map(|c| c.package.name.as_str()).collect();
        assert_eq!(names, vec!["c", "b", "a"]);
    }

    #[test]
    fn resolves_virtual_provides() {
        let universe = universe_of(vec![
            pkg("app", "1.0-1", &["sh"], &[]),
            pkg("bash", "5.2-1", &[], &["sh"]),
        ]);
        let res = resolve(&universe, &["app"], &[]);
        assert!(res.missing.is_empty());
        let names: Vec<&str> = res.order.iter().map(|c| c.package.name.as_str()).collect();
        assert_eq!(names, vec!["bash", "app"]);
    }

    #[test]
    fn reports_missing_dependencies() {
        let universe = universe_of(vec![pkg("a", "1.0-1", &["nonexistent"], &[])]);
        let res = resolve(&universe, &["a"], &[]);
        assert_eq!(res.missing, vec!["nonexistent".to_string()]);
        assert_eq!(res.order.len(), 1);
    }

    #[test]
    fn skips_already_installed_dependency() {
        let universe = universe_of(vec![
            pkg("a", "2.0-1", &["b>=1.0"], &[]),
            pkg("b", "2.0-1", &[], &[]),
        ]);
        let installed = vec![pkg("b", "1.5-1", &[], &[])];
        let res = resolve(&universe, &["a"], &installed);
        // installed b (1.5-1) satisfies b>=1.0, so it's skipped as a dependency.
        assert!(res.missing.is_empty());
        let names: Vec<&str> = res.order.iter().map(|c| c.package.name.as_str()).collect();
        assert_eq!(names, vec!["a"]);
    }

    #[test]
    fn virtual_dep_satisfied_by_installed_provider_is_not_reinstalled() {
        // Mirrors firefox depending on `ttf-font`, already satisfied by
        // whatever font package is installed — a fresh sync candidate for
        // the virtual name must NOT be pulled in.
        let universe = universe_of(vec![
            pkg("app", "1.0-1", &["ttf-font"], &[]),
            pkg("some-other-font", "1.0-1", &[], &["ttf-font"]),
        ]);
        let installed = vec![pkg("already-installed-font", "1.0-1", &[], &["ttf-font"])];
        let res = resolve(&universe, &["app"], &installed);
        assert!(res.missing.is_empty());
        let names: Vec<&str> = res.order.iter().map(|c| c.package.name.as_str()).collect();
        assert_eq!(names, vec!["app"]);
    }

    #[test]
    fn explicit_target_included_even_if_already_installed() {
        let universe = universe_of(vec![pkg("a", "2.0-1", &[], &[])]);
        let installed = vec![pkg("a", "2.0-1", &[], &[])];
        let res = resolve(&universe, &["a"], &installed);
        assert!(res.missing.is_empty());
        let names: Vec<&str> = res.order.iter().map(|c| c.package.name.as_str()).collect();
        assert_eq!(names, vec!["a"]);
    }

    #[test]
    fn handles_dependency_cycles_without_hanging() {
        let universe = universe_of(vec![
            pkg("a", "1.0-1", &["b"], &[]),
            pkg("b", "1.0-1", &["a"], &[]),
        ]);
        let res = resolve(&universe, &["a"], &[]);
        assert!(res.missing.is_empty());
        assert_eq!(res.order.len(), 2);
    }
}
