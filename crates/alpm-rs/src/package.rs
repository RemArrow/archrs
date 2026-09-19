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

/// Parse a `.PKGINFO` file's contents — the metadata makepkg embeds in
/// every built package archive. Different format from the local/sync
/// db's `desc` file (`key = value` lines, repeated keys for array
/// fields, `#`-prefixed comments) despite describing the same package.
pub fn parse_pkginfo(text: &str) -> Package {
    let mut pkg = Package::default();
    let mut licenses = Vec::new();
    let mut depends = Vec::new();
    let mut optdepends = Vec::new();
    let mut makedepends = Vec::new();
    let mut conflicts = Vec::new();
    let mut provides = Vec::new();
    let mut replaces = Vec::new();
    let mut groups = Vec::new();

    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().to_string();

        match key {
            "pkgname" => pkg.name = value,
            "pkgbase" => pkg.base = Some(value),
            "pkgver" => pkg.version = value,
            "pkgdesc" => pkg.description = Some(value),
            "url" => pkg.url = Some(value),
            "arch" => pkg.arch = Some(value),
            "packager" => pkg.packager = Some(value),
            "size" => pkg.size = value.parse().ok(),
            "builddate" => pkg.build_date = value.parse().ok(),
            "license" => licenses.push(value),
            "depend" => depends.push(value),
            "optdepend" => optdepends.push(value),
            "makedepend" => makedepends.push(value),
            "conflict" => conflicts.push(value),
            "provides" => provides.push(value),
            "replaces" => replaces.push(value),
            "group" => groups.push(value),
            _ => {} // xdata, checkdepend, etc. — not tracked yet
        }
    }

    pkg.licenses = licenses;
    pkg.depends = depends;
    pkg.optdepends = optdepends;
    pkg.makedepends = makedepends;
    pkg.conflicts = conflicts;
    pkg.provides = provides;
    pkg.replaces = replaces;
    pkg.groups = groups;
    pkg
}

/// Render a `.PKGINFO` file's contents for a freshly built package,
/// matching the format real makepkg writes (see `parse_pkginfo`'s test
/// for a real-world example) closely enough for pacman/alpm-rs to read
/// back correctly — not necessarily byte-identical to makepkg's own
/// output (e.g. it doesn't emit `xdata`).
pub fn write_pkginfo(pkg: &Package, packager: &str, build_date: i64, size: u64) -> String {
    let mut out = String::new();
    out.push_str("# Generated by makepkg-rs\n");
    out.push_str(&format!("pkgname = {}\n", pkg.name));
    out.push_str(&format!(
        "pkgbase = {}\n",
        pkg.base.as_deref().unwrap_or(&pkg.name)
    ));
    out.push_str(&format!("pkgver = {}\n", pkg.version));
    if let Some(desc) = &pkg.description {
        out.push_str(&format!("pkgdesc = {desc}\n"));
    }
    if let Some(url) = &pkg.url {
        out.push_str(&format!("url = {url}\n"));
    }
    out.push_str(&format!("builddate = {build_date}\n"));
    out.push_str(&format!("packager = {packager}\n"));
    out.push_str(&format!("size = {size}\n"));
    out.push_str(&format!(
        "arch = {}\n",
        pkg.arch.as_deref().unwrap_or("any")
    ));
    for license in &pkg.licenses {
        out.push_str(&format!("license = {license}\n"));
    }
    for replace in &pkg.replaces {
        out.push_str(&format!("replaces = {replace}\n"));
    }
    for conflict in &pkg.conflicts {
        out.push_str(&format!("conflict = {conflict}\n"));
    }
    for provide in &pkg.provides {
        out.push_str(&format!("provides = {provide}\n"));
    }
    for group in &pkg.groups {
        out.push_str(&format!("group = {group}\n"));
    }
    for dep in &pkg.depends {
        out.push_str(&format!("depend = {dep}\n"));
    }
    for dep in &pkg.optdepends {
        out.push_str(&format!("optdepend = {dep}\n"));
    }
    for dep in &pkg.makedepends {
        out.push_str(&format!("makedepend = {dep}\n"));
    }
    out
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
        assert_eq!(
            pkg.description.as_deref(),
            Some("File archiver for extremely high compression")
        );
        assert_eq!(pkg.size, Some(6783359));
        assert_eq!(pkg.licenses, vec!["LGPL-2.1-or-later", "BSD-3-Clause"]);
        assert_eq!(pkg.depends, vec!["sh", "libgcc", "libstdc++", "glibc"]);
        assert_eq!(pkg.provides, vec!["p7zip"]);
    }

    #[test]
    fn parses_real_pkginfo_format() {
        // Taken verbatim from a real 7zip-26.03-1-x86_64.pkg.tar.zst's
        // .PKGINFO, extracted from this system's own pacman cache.
        let text = "\
# Generated by makepkg 7.1.0
# using fakeroot version 1.37.2
pkgname = 7zip
pkgbase = 7zip
xdata = pkgtype=pkg
pkgver = 26.03-1
pkgdesc = File archiver for extremely high compression
url = https://www.7-zip.org
builddate = 1788563316
packager = George Rawlinson <grawlinson@archlinux.org>
size = 6783359
arch = x86_64
license = LGPL-2.1-or-later AND LicenseRef-UnRAR AND BSD-3-Clause AND BSD-2-Clause
replaces = p7zip
conflict = p7zip
provides = p7zip
depend = sh
depend = libgcc
depend = libstdc++
depend = glibc
makedepend = uasm
";
        let pkg = parse_pkginfo(text);
        assert_eq!(pkg.name, "7zip");
        assert_eq!(pkg.base.as_deref(), Some("7zip"));
        assert_eq!(pkg.version, "26.03-1");
        assert_eq!(
            pkg.description.as_deref(),
            Some("File archiver for extremely high compression")
        );
        assert_eq!(pkg.url.as_deref(), Some("https://www.7-zip.org"));
        assert_eq!(pkg.arch.as_deref(), Some("x86_64"));
        assert_eq!(pkg.size, Some(6783359));
        assert_eq!(pkg.build_date, Some(1788563316));
        assert_eq!(pkg.replaces, vec!["p7zip"]);
        assert_eq!(pkg.conflicts, vec!["p7zip"]);
        assert_eq!(pkg.provides, vec!["p7zip"]);
        assert_eq!(pkg.depends, vec!["sh", "libgcc", "libstdc++", "glibc"]);
        assert_eq!(pkg.makedepends, vec!["uasm"]);
    }

    #[test]
    fn pkginfo_round_trips_through_write_and_parse() {
        let pkg = Package {
            name: "hello".to_string(),
            version: "1.0-1".to_string(),
            base: Some("hello".to_string()),
            description: Some("A test package".to_string()),
            url: Some("https://example.com".to_string()),
            arch: Some("x86_64".to_string()),
            licenses: vec!["MIT".to_string()],
            depends: vec!["glibc".to_string()],
            provides: vec!["hello-cmd".to_string()],
            ..Package::default()
        };
        let text = write_pkginfo(&pkg, "archrs <archrs@localhost>", 1_700_000_000, 4096);
        let parsed = parse_pkginfo(&text);
        assert_eq!(parsed.name, pkg.name);
        assert_eq!(parsed.version, pkg.version);
        assert_eq!(parsed.base, pkg.base);
        assert_eq!(parsed.description, pkg.description);
        assert_eq!(parsed.url, pkg.url);
        assert_eq!(parsed.arch, pkg.arch);
        assert_eq!(parsed.licenses, pkg.licenses);
        assert_eq!(parsed.depends, pkg.depends);
        assert_eq!(parsed.provides, pkg.provides);
        assert_eq!(parsed.build_date, Some(1_700_000_000));
        assert_eq!(parsed.size, Some(4096));
    }
}
