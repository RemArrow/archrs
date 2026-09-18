use std::path::Path;

/// A package's metadata, parsed from a pacman-format `desc` file
/// (`%NAME%\nvalue\n\n%VERSION%\nvalue\n\n...`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub base: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub arch: Option<String>,
    pub packager: Option<String>,
    pub size: Option<u64>,
    pub licenses: Vec<String>,
    pub depends: Vec<String>,
    pub optdepends: Vec<String>,
    pub makedepends: Vec<String>,
    pub conflicts: Vec<String>,
    pub provides: Vec<String>,
    pub replaces: Vec<String>,
    pub groups: Vec<String>,
    pub build_date: Option<i64>,
    pub install_date: Option<i64>,
    pub reason: Option<String>,

    // Sync-DB-only fields (absent for locally-installed packages).
    pub filename: Option<String>,
    pub csize: Option<u64>,
    pub md5sum: Option<String>,
    pub sha256sum: Option<String>,
    pub pgpsig: Option<String>,
}

/// Parse a `desc` file's contents. Format is a sequence of
/// `%FIELD%` headers each followed by one or more value lines, blocks
/// separated by a blank line.
pub fn parse_desc(text: &str) -> Package {
    let mut pkg = Package::default();
    let mut lines = text.lines().peekable();

    while let Some(line) = lines.next() {
        let Some(field) = line.strip_prefix('%').and_then(|s| s.strip_suffix('%')) else {
            continue;
        };

        let mut values = Vec::new();
        while let Some(next) = lines.peek() {
            if next.is_empty() {
                break;
            }
            values.push(lines.next().unwrap().to_string());
        }
        let joined = values.join("\n");

        match field {
            "NAME" => pkg.name = joined,
            "VERSION" => pkg.version = joined,
            "BASE" => pkg.base = Some(joined),
            "DESC" => pkg.description = Some(joined),
            "URL" => pkg.url = Some(joined),
            "ARCH" => pkg.arch = Some(joined),
            "PACKAGER" => pkg.packager = Some(joined),
            "SIZE" | "ISIZE" => pkg.size = joined.parse().ok(),
            "LICENSE" => pkg.licenses = values,
            "DEPENDS" => pkg.depends = values,
            "OPTDEPENDS" => pkg.optdepends = values,
            "MAKEDEPENDS" => pkg.makedepends = values,
            "CONFLICTS" => pkg.conflicts = values,
            "PROVIDES" => pkg.provides = values,
            "REPLACES" => pkg.replaces = values,
            "GROUPS" => pkg.groups = values,
            "BUILDDATE" => pkg.build_date = joined.parse().ok(),
            "INSTALLDATE" => pkg.install_date = joined.parse().ok(),
            "REASON" => pkg.reason = Some(joined),
            "FILENAME" => pkg.filename = Some(joined),
            "CSIZE" => pkg.csize = joined.parse().ok(),
            "MD5SUM" => pkg.md5sum = Some(joined),
            "SHA256SUM" => pkg.sha256sum = Some(joined),
            "PGPSIG" => pkg.pgpsig = Some(joined),
            _ => {}
        }
    }

    pkg
}

pub fn parse_desc_file(path: impl AsRef<Path>) -> std::io::Result<Package> {
    let text = std::fs::read_to_string(path)?;
    Ok(parse_desc(&text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_desc_format() {
        let text = "\
%NAME%
7zip

%VERSION%
26.03-1

%DESC%
File archiver for extremely high compression

%URL%
https://www.7-zip.org

%ARCH%
x86_64

%SIZE%
6783359

%LICENSE%
LGPL-2.1-or-later
BSD-3-Clause

%DEPENDS%
sh
libgcc
libstdc++
glibc

%PROVIDES%
p7zip
";
        let pkg = parse_desc(text);
        assert_eq!(pkg.name, "7zip");
        assert_eq!(pkg.version, "26.03-1");
        assert_eq!(pkg.description.as_deref(), Some("File archiver for extremely high compression"));
        assert_eq!(pkg.size, Some(6783359));
        assert_eq!(pkg.licenses, vec!["LGPL-2.1-or-later", "BSD-3-Clause"]);
        assert_eq!(pkg.depends, vec!["sh", "libgcc", "libstdc++", "glibc"]);
        assert_eq!(pkg.provides, vec!["p7zip"]);
    }
}
