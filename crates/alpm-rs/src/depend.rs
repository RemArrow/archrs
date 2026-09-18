use std::cmp::Ordering;
use std::fmt;

use crate::version::vercmp;

/// A dependency constraint operator, as written in a depend string like
/// `glibc>=2.34`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepOp {
    Eq,
    Ge,
    Le,
    Gt,
    Lt,
}

impl DepOp {
    fn satisfied_by(self, ord: Ordering) -> bool {
        match self {
            DepOp::Eq => ord == Ordering::Equal,
            DepOp::Ge => ord != Ordering::Less,
            DepOp::Le => ord != Ordering::Greater,
            DepOp::Gt => ord == Ordering::Greater,
            DepOp::Lt => ord == Ordering::Less,
        }
    }
}

impl fmt::Display for DepOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            DepOp::Eq => "=",
            DepOp::Ge => ">=",
            DepOp::Le => "<=",
            DepOp::Gt => ">",
            DepOp::Lt => "<",
        };
        f.write_str(s)
    }
}

/// A parsed dependency string, e.g. `glibc`, `glibc>=2.34`, or
/// `libacl.so=1-64`. `desc` in an `%OPTDEPENDS%` entry (the text after a
/// colon) is dropped — optional deps are treated as plain names for
/// satisfaction purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Depend {
    pub name: String,
    pub constraint: Option<(DepOp, String)>,
}

impl Depend {
    pub fn parse(raw: &str) -> Depend {
        // optdepends carry a trailing "<name>: <reason>" — strip it.
        let raw = raw.split_once(':').map(|(n, _)| n).unwrap_or(raw).trim();

        for op in [">=", "<=", "=", ">", "<"] {
            if let Some((name, ver)) = raw.split_once(op) {
                let op = match op {
                    ">=" => DepOp::Ge,
                    "<=" => DepOp::Le,
                    "=" => DepOp::Eq,
                    ">" => DepOp::Gt,
                    "<" => DepOp::Lt,
                    _ => unreachable!(),
                };
                return Depend {
                    name: name.to_string(),
                    constraint: Some((op, ver.to_string())),
                };
            }
        }

        Depend {
            name: raw.to_string(),
            constraint: None,
        }
    }

    /// Does a provided name/version pair (from a package's own name+version,
    /// or one of its `%PROVIDES%` entries) satisfy this dependency?
    pub fn satisfied_by(&self, provided_name: &str, provided_version: Option<&str>) -> bool {
        if provided_name != self.name {
            return false;
        }
        match (&self.constraint, provided_version) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some((op, want)), Some(have)) => op.satisfied_by(vercmp(have, want)),
        }
    }
}

impl fmt::Display for Depend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.constraint {
            Some((op, ver)) => write!(f, "{}{op}{ver}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

/// A `%PROVIDES%` entry, e.g. `p7zip` or `libacl.so=1-64`.
pub fn parse_provide(raw: &str) -> (String, Option<String>) {
    match raw.split_once('=') {
        Some((name, ver)) => (name.to_string(), Some(ver.to_string())),
        None => (raw.to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_and_versioned_deps() {
        assert_eq!(
            Depend::parse("glibc"),
            Depend {
                name: "glibc".into(),
                constraint: None
            }
        );
        assert_eq!(
            Depend::parse("glibc>=2.34"),
            Depend {
                name: "glibc".into(),
                constraint: Some((DepOp::Ge, "2.34".into()))
            }
        );
        assert_eq!(
            Depend::parse("libacl.so=1-64"),
            Depend {
                name: "libacl.so".into(),
                constraint: Some((DepOp::Eq, "1-64".into()))
            }
        );
    }

    #[test]
    fn strips_optdepend_description() {
        let d = Depend::parse("systemd: for machinectl support");
        assert_eq!(d.name, "systemd");
        assert_eq!(d.constraint, None);
    }

    #[test]
    fn version_constraints_are_checked() {
        let d = Depend::parse("glibc>=2.34");
        assert!(d.satisfied_by("glibc", Some("2.40-1")));
        assert!(!d.satisfied_by("glibc", Some("2.20-1")));
        assert!(!d.satisfied_by("glibc", None));
        assert!(!d.satisfied_by("musl", Some("2.40-1")));
    }

    #[test]
    fn bare_dep_ignores_version() {
        let d = Depend::parse("sh");
        assert!(d.satisfied_by("sh", None));
        assert!(d.satisfied_by("sh", Some("5.2-1")));
    }
}
