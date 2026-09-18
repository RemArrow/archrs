# archrs — Arch Linux userland in Rust

Goal: replace the Arch Linux userland, component by component, with Rust
implementations, while continuing to run on the stock Linux kernel. This is a
long-horizon project, tracked in phases below.

## Out of scope (not "porting to Rust" in any real sense)
- **Linux kernel** — Arch runs the upstream Linux kernel. A full from-scratch
  Rust kernel is a separate, decade-scale effort (see Redox OS), and it would
  no longer be "Arch" — it'd be a different OS. Not attempted here.
- **glibc** — no drop-in Rust replacement exists for glibc-linked binaries.
  We build against glibc/musl as a libc, same as any other Rust program on
  Linux; we don't reimplement libc itself.

## Phases

### Phase 1 — `alpm-rs` / `pacman-rs` (in progress)
Reimplement libalpm + pacman in Rust, read-only first:
- [x] Parse `pacman.conf`
- [x] Parse local package DB (`/var/lib/pacman/local/*/desc`)
- [x] `pacman -Q` / `-Qi` / `-Ql` equivalents (query installed packages)
- [x] Parse sync DBs (`/var/lib/pacman/sync/*.db`) — sniffs the actual
      compression (gzip, zstd, or none) instead of assuming gzip; found by
      downloading a real `.db` from a Manjaro mirror, which turned out to
      be a plain uncompressed tar even though this system's own cached
      copy is gzip'd
- [x] Version comparison (`vercmp`) — ported from libalpm's `version.c`,
      verified against the real `vercmp` binary over 4000+ generated and
      real-world version-string pairs (see `scripts/fuzz_vercmp*.sh`)
- [x] Dependency resolution graph, including virtual `provides` (e.g.
      `ttf-font`) and version-constrained deps (`glibc>=2.34`) — `-Sp`
      output verified against real `pacman -Sp` for both a 12-package tree
      (gimp) and a 63-package tree (blender): identical package sets and,
      for gimp, identical install order
- [x] `-Si` (sync info) equivalent
- [x] Real install (`-S`): resolves mirrors from `pacman.conf`/mirrorlist,
      downloads over HTTPS, verifies sha256 + GPG signature (via the
      system `gpg` binary against pacman's own keyring — signature
      verification is security-critical code with a battle-tested
      implementation already on the system, so shelling out to it beats
      a from-scratch OpenPGP reimplementation), extracts the zstd/tar
      package, and writes a real local-db entry. `--root`/`--dbpath`/
      `--cachedir` let it target a sandbox instead of the live system.
      Verified end-to-end against real Manjaro mirrors: installed `acl`
      and its full dependency chain (linux-api-headers, tzdata, iana-etc,
      filesystem, glibc) into a sandboxed root — real GPG signatures from
      actual Arch/Manjaro developer keys verified successfully, hard
      links (tzdata aliases many zoneinfo files together) came out
      correctly linked, and the resulting sandbox's `pacman-rs -Q`/`-Qi`/
      `-Ql` matched real pacman's local-db format field-for-field.
- [x] Sync DB refresh (`-Sy`) and system upgrade (`-Su`): `-Sy` downloads
      a fresh `<repo>.db` per configured repo; `-Su` diffs installed vs.
      sync versions with `vercmp`, resolves the upgrade set (pulling in
      any new dependencies), and installs through the same fetch/verify/
      extract pipeline as `-S`, preserving each package's original
      explicit/dependency reason and cleaning up the superseded version's
      local-db directory (otherwise `-Q` would show the package twice —
      caught by testing an upgrade in the sandbox before calling it done).
- [x] Package removal (`-R`): deletes a package's files deepest-path-first
      (so shared directories that empty out get removed, but ones still
      owned by another package are left alone) and its local-db entry.
      Refuses to remove a package another installed package still depends
      on (by name or by `provides`) unless `--nodeps` is passed.
      Verified in the sandbox end-to-end: `-Sy` against real mirrors (and
      found a real bug doing it — see below), `-Su` correctly detected and
      upgraded a rolled-back `acl` while leaving unrelated packages alone,
      `-R glibc` was correctly refused because `acl` still depended on it,
      and `-R acl` deleted exactly the right files while leaving
      still-shared directories intact.
- [x] `-Rs` (recursively remove now-unneeded dependencies): repeatedly
      sweeps up each removed package's own dependencies that were
      installed as a dependency (not explicitly) and would be left with
      no remaining dependent — verified in the sandbox two ways: removing
      `acl` alone swept its entire 6-package dependency chain, but
      installing `attr` alongside it (sharing glibc/filesystem/etc.) and
      then `-Rs acl` left every shared dependency in place and removed
      only `acl` itself, exactly matching real pacman's semantics.

Phase 1 is functionally complete: `pacman-rs` can query, resolve, install,
upgrade, and remove packages against real Arch/Manjaro infrastructure.

### Phase 2 — coreutils-rs (in progress)
Rust reimplementations of core utilities used by the base install
(ls, cp, mv, cat, grep, ...). Follow the prior art of `uutils/coreutils`
rather than reinventing; adapt/vendor where sensible.

- [x] `coreutils-rs` multicall binary vendoring the real `uu_*` crates
      (the same ones the upstream `coreutils` binary is built from) for
      `ls`, `cat`, `cp`, `mv`, `rm`, `mkdir`, `echo`, `pwd`, `touch`, `wc`,
      `head`, `tail`, `true`, `false` — dispatch only, no reimplementation,
      since reimplementing what uutils already did well isn't "porting",
      it's duplicating. Every `uu_*` crate's public `uumain` is generated
      by the `#[uucore::main]` attribute macro into a plain
      `fn(impl Args) -> i32` that already handles error printing and the
      exit code, so the dispatcher just forwards argv and the return code.
      Supports both a busybox-style symlink install (`ls -> coreutils-rs`,
      argv[0] names the utility) and `coreutils-rs <utility> [args...]`
      for trying a utility out without installing symlinks.
      Verified byte-identical output against real GNU coreutils for
      `ls -la`, `cat`, `wc`, `head -n`, `tail -n`, and `cp` in a sandbox;
      `mkdir -p`, `touch`, `mv`, and `rm` verified functionally.
      Known gap: `--help`/`--version` text shows untranslated placeholder
      strings (`ls-about`, `ls-usage`) — the upstream crates expect their
      Fluent localization assets bundled by the parent `coreutils` build,
      which we don't do yet.
- [ ] More utilities as the base install needs them (grep, sed, find,
      tar, gzip, less, ...) — same vendor-and-dispatch approach.
- [ ] Bundle/enable the Fluent localization assets so `--help`/`--version`
      render real text instead of placeholder keys.

### Phase 3 — init & service management
Arch uses systemd. A from-scratch Rust init is a huge surface (cgroups,
dbus, unit files, udev). Realistic approach: scope down to a minimal
Rust init for a custom "archrs" live image, not a systemd replacement
for real installs.

### Phase 4 — shell & base-devel toolchain
bash replacement, makepkg equivalent, build tooling.

## Non-goals
Rewriting every package in the Arch repos (tens of thousands of packages,
most already upstream projects in their own languages) is not a software
task — it's not being attempted. "Fully in Rust" here means: Rust userland
core + Linux kernel, same as how "Arch Linux" is glibc/bash/systemd +
Linux kernel today.
