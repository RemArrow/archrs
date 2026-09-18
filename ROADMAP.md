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
- [x] `-U` (install a local package file directly, e.g. one built by
      `makepkg-rs` — see Phase 4): reads the archive's own `.PKGINFO`
      (a different format from the local/sync db's `desc` file despite
      describing the same package — added `parse_pkginfo`/
      `write_pkginfo`/`read_pkginfo` to `alpm-rs` to handle it) rather
      than resolving metadata from a sync db entry, then goes through
      the same extraction/local-db path `-S` already used. Verified
      end-to-end against a real cached Arch package
      (`7zip-26.03-1-x86_64.pkg.tar.zst`): installed it with `-U` into a
      sandboxed root and diffed the result against real `pacman -U`
      installing the same archive into its own sandboxed root (with
      `--nodeps` since neither sandbox has the dependency chain
      installed) — identical file lists, and byte-identical installed
      binaries (`md5sum` match).

Phase 1 is functionally complete: `pacman-rs` can query, resolve, install,
upgrade, remove, and directly install local package files against real
Arch/Manjaro infrastructure.

### Phase 2 — coreutils-rs (functionally complete)
Rust reimplementations of core utilities used by the base install
(ls, cp, mv, cat, grep, ...). Follow the prior art of `uutils/coreutils`
rather than reinventing; adapt/vendor where sensible.

- [x] `coreutils-rs` multicall binary vendoring the real `uu_*` crates
      (the same ones the upstream `coreutils` binary is built from) —
      dispatch only, no reimplementation, since reimplementing what
      uutils already did well isn't "porting", it's duplicating. Every
      `uu_*` crate's public `uumain` is generated by the `#[uucore::main]`
      attribute macro into a plain `fn(impl Args) -> i32` that already
      handles error printing and the exit code, so the dispatcher just
      forwards argv and the return code. Supports both a busybox-style
      symlink install (`ls -> coreutils-rs`, argv[0] names the utility)
      and `coreutils-rs <utility> [args...]` for trying a utility out
      without installing symlinks.
- [x] Essentially all of real GNU coreutils' ~99 utilities are wired up:
      `ls cat cp mv rm mkdir echo pwd touch wc head tail true false
      chmod chown chgrp ln rmdir mkfifo mknod du df sort uniq cut tr tee
      dd dirname basename realpath readlink sync sleep date id whoami
      who uname env printf seq shuf split join paste comm expand
      unexpand fold fmt nl od base64 base32 md5sum sha1sum sha256sum
      sha512sum mktemp install stat test [ expr yes nice nohup timeout
      kill factor numfmt tsort csplit shred link unlink vdir dir
      dircolors groups logname tty users stdbuf hostid arch nproc
      printenv pathchk pinky sum cksum`. `test`/`[` needed no special
      casing — `uu_test` already branches on `uucore::util_name()` to
      decide whether it needs a trailing `]`, matching real coreutils'
      own dual-symlink trick.
      Spot-verified byte-identical output against real GNU coreutils for
      `ls -la`, `cat`, `wc`, `head -n`, `tail -n`, `cp`, `sort`, `uniq`,
      `cut`, `tr`, `seq`, `base64`, `sha256sum`, `basename`, `dirname`,
      `id -u`, `uname -m`, `expr`, `stat -c %s`; `mkdir -p`, `touch`,
      `mv`, `rm`, `chmod`, `ln -s`, `du`, `test`/`[` verified functionally.
- [x] `--help`/`--version` render real text (release builds only) instead
      of raw Fluent keys like `ls-about`. Root cause: each `uu_*` crate
      needs `uucore::locale::setup_localization(name)` called with a
      filesystem path to its `locales/*.ftl` files, which the upstream
      `coreutils` monorepo satisfies via a layout (`src/uu/<name>/locales`
      next to `src/uucore/locales`) that doesn't exist when pulling `uu_*`
      as ordinary crates.io deps like we do — `uucore`'s own
      embedded-locale fallback also silently no-ops outside that layout.
      Fixed with a `build.rs` (see its own comments for the full
      reverse-engineered detail) that uses `cargo_metadata` to find each
      dependency's source dir and copies its `locales/*.ftl` next to the
      compiled binary — including two non-obvious extra copies:
      `checksum_common`'s locale (shared by the `*sum` utilities) and
      uucore's own common-strings bundle, which a hardcoded 3-parent
      directory walk inside uucore expects one level *above* the target
      dir rather than under it. `dir`/`vdir` (thin `ls` wrappers with no
      locale files of their own) correctly fall back to `ls`'s bundle via
      `uucore::get_canonical_util_name`, matching real coreutils.
      This leans on private uucore internals with no stability guarantee,
      so a future uucore 0.12.x patch could silently break it again —
      worth rechecking after any version bump.
- [ ] Utilities outside real coreutils that a base install still needs
      (grep, sed, find, tar, gzip, less, ...) come from separate GNU
      projects, not `uutils/coreutils` — each needed its own sourcing
      decision instead of a ready `uu_*` crate:
  - [x] `find`, `xargs`, `locate`, `updatedb` — vendored from the
        `findutils` crate, GNU findutils' own official uutils Rust port
        (same organization as `uutils/coreutils`, published the same
        way). Verified `find -name`/`-type` and `xargs` byte-identical
        against real GNU findutils.
  - [x] `tar` — GNU tar has no Rust port to vendor, so this is our own
        thin CLI (`src/tar_cmd.rs`) over the `tar`/`flate2`/`zstd` crates
        (the same libraries `alpm-rs` already uses for real package
        archives). Covers `-c`/`-x`/`-t` with gzip/zstd compression,
        both the old bundled-flag style (`tar xvf a.tar`) and normal
        `-xvf`. Not implemented: bzip2/xz (no vendored crate yet),
        incremental archives, extracting a subset of named members.
        Verified round-trip and cross-interop with real GNU tar in both
        directions (our archive → real tar extract, and vice versa),
        including `.tar.gz`. Known cosmetic gap: `-t` listing doesn't
        always show a trailing `/` on directory entries the way real
        tar's `-t` does, because the `tar` crate's own `append_dir_all`
        doesn't consistently store one for nested directories — harmless,
        since extraction itself was verified byte-identical.
  - [x] `gzip`/`gunzip`/`zcat` — likewise no Rust port to vendor; our own
        thin CLI (`src/gzip_cmd.rs`) over `flate2`, with `gzip`'s usual
        argv[0]-based aliasing (`gunzip`/`zcat` imply decompression,
        `zcat` also implies `-c`). Verified round-trip and cross-interop
        with real gzip/gunzip in both directions.
  - [x] `grep` — vendored BurntSushi's `grep-searcher`/`grep-regex`/
        `grep-matcher` crates (the actual libraries ripgrep is built
        from) with a hand-written GNU-grep-compatible CLI layer
        (`src/grep_cmd.rs`), since no ready `grep`-flag-compatible
        binary crate exists. Covers `-i -v -n -c -l -L -r -R -w -x -o
        -F -E -H -h`, files/directories/stdin. Pattern syntax is the
        `regex` crate's own (ERE-like, no backreferences) rather than
        true POSIX BRE — GNU grep's default mode has different escaping
        (`\(` `\)` for groups) the regex crate doesn't support; noted as
        a real behavior deviation for patterns that lean on it, though
        everyday patterns (literals, character classes, `*`/`.`/`^`/`$`)
        behave the same either way. Not implemented: `-A`/`-B`/`-C`
        context lines, `-P` (PCRE).
        Verified byte-identical against real GNU grep 3.12 across all
        of the above flags, multi-file search (with correct filename
        prefixing), recursive directory search, and stdin. One
        surprise along the way: this system's own `grep` shell
        alias/wrapper resolves to `ugrep`, not GNU grep, and ugrep's
        own multi-file output ordering is genuinely non-deterministic
        (parallel file scanning) — had to test against `/usr/bin/grep`
        directly to get an authoritative comparison; our own multi-file
        ordering is deterministic (command-line argument order), which
        also matches true GNU grep's documented behavior.
  - [x] `sed` — no vendor target existed (GNU sed's scripting language
        has no existing Rust implementation to draw on), so this scope
        decision was made explicitly rather than defaulting to "vendor":
        a hand-written interpreter (`src/sed_cmd.rs`) covering
        addresses (line number, `$`, `/regex/`, `addr1,addr2` ranges,
        `!` negation), `s/pat/repl/flags` (`g`/`i`/`I`/`p`, any
        non-backslash delimiter, `&`/`\1`-`\9`/`\&`/`\\` in
        replacements) built on the `regex` crate, plus `p`/`d`/`q`,
        `-n`, `-e` (repeatable), and `-i[SUFFIX]`. Explicitly not
        implemented: hold space, branches/labels, multi-line commands
        (`N`/`D`/`P`), `y///`, `a`/`i`/`c`. Whole input is read into
        memory rather than streamed (fine at this scope).
        Verified byte-identical against real GNU sed 4.10 across basic
        `s///`, `g`/`i` flags, `-n`+`p`, line/regex/range/`$`
        addressing, negation, `q`, capture-group backreferences (`-E`),
        `&`, a custom delimiter, multiple `-e`, `-i` and `-i.bak`, and
        stdin.
  - [x] `less` — vendored the `minus` crate (an actual terminal-pager
        library — raw terminal mode, scroll state, search highlighting
        aren't worth hand-rolling), via its `static_output` mode
        (`src/less_cmd.rs`). Scrolling and `/`-search come from `minus`
        itself. Not implemented: less's own feature set beyond that
        (multiple files with `:n`/`:p`, marks, `-N` line numbers, etc.)
        — `minus` doesn't expose those as building blocks. Falls back
        to printing content directly when stdout isn't a terminal.
        Verified two ways: the non-terminal fallback path is
        byte-identical to `cat` for both a file argument and stdin.
        The actual interactive pager can't be driven from this sandbox
        the way the other utilities were regression-tested (it needs a
        real terminal, not just a piped stdout), so instead it was
        exercised over a genuine pseudo-tty (Python's `pty.openpty`):
        confirmed it enters the alternate screen buffer and enables
        mouse tracking on startup, then — after being sent `q` — cleanly
        disables mouse tracking and exits the alternate screen buffer,
        i.e. it starts and shuts down correctly. Actual line rendering
        wasn't visible in that capture (the fake pty has no real window
        size), so full visual/scrolling behavior is unverified beyond
        that — worth a real terminal check before relying on it.

### Phase 3 — init & service management (started)
Arch uses systemd. A from-scratch Rust init is a huge surface (cgroups,
dbus, unit files, udev). Realistic approach: scope down to a minimal
Rust init for a custom "archrs" live image, not a systemd replacement
for real installs.

- [x] `archrs-init`: a minimal PID-1 binary. Mounts `/proc`, `/sys`,
      `/dev` (after making the mount namespace's root private, so the
      new mounts don't propagate back to whatever namespace it inherited
      from — standard practice for any init/container-runtime doing this,
      not archrs-specific). Spawns a fixed list of services from
      `/etc/archrs-init.conf` (one command per line; `ARCHRS_INIT_CONF`
      env var overrides the path, used for testing), falling back to
      `/bin/sh` as a rescue shell if that file doesn't exist. Reaps every
      child for as long as it runs — the part that's actually specific to
      being PID 1: any process whose original parent exits first gets
      reparented to init by the kernel, and without an unconditional
      `waitpid(-1, ...)` loop those become permanent zombies the moment
      they exit. SIGTERM/SIGINT trigger a teardown (SIGTERM to every
      process in the namespace, a 500ms grace period, then SIGKILL) and
      clean exit.
      Verified for real as PID 1 — not just logically — using
      `unshare --user --pid --mount --fork`, which gives an unprivileged
      process a genuine fresh PID+mount namespace: confirmed a spawned
      service that itself backgrounds a subprocess and exits immediately
      (`(sleep 2 &); exit 0`) correctly reparents the orphaned `sleep` to
      archrs-init, which reaps it ~2s later exactly as expected — the
      orphan-reaping behavior that's the whole reason PID-1 code differs
      from ordinary process supervision. Also confirmed SIGTERM-triggered
      shutdown tears everything down and exits cleanly.
      Known gap: mounting `/proc`/`/sys`/`/dev` itself couldn't be
      verified in this environment — the sandboxed dev container's user
      namespace returned EPERM for all three (likely a seccomp/LSM
      restriction on the outer container, not a kernel limitation of
      unprivileged user namespaces in general). The mount calls
      themselves use the same flags and MS_PRIVATE-first sequencing real
      init systems and container runtimes use, but this needs a real
      boot or a more permissive sandbox to confirm.
- [x] Service ordering, restart policies, and reboot/poweroff. Service
      list lines take an optional prefix: `wait <cmd>` blocks until it
      exits before moving to the next line (the ordering primitive —
      one-shot setup steps other services depend on), `respawn <cmd>`
      restarts it whenever it exits as long as the system isn't
      shutting down, and a bare command is the original start-once-and-
      leave-it behavior. SIGUSR1/SIGUSR2 trigger the same teardown as
      SIGTERM/SIGINT but then call the real `reboot(2)` syscall
      (`RB_AUTOBOOT`/`RB_POWER_OFF`) — no separate `reboot`/`poweroff`
      command exists yet, just these two signals, since there's no IPC
      mechanism for anything richer.
      Verified `wait` ordering and `respawn` for real under
      `unshare --user --pid --mount --fork`: two `wait`-prefixed setup
      steps run and complete strictly in order before the async service
      starts, and a `respawn`-prefixed short-lived process gets
      restarted every time it exits, correctly stopping once shutdown
      begins. Verified SIGUSR1/SIGUSR2 for real too: both correctly run
      the shutdown sequence, then attempt the actual `reboot(2)` syscall
      with the right mode flag, observed via a backgrounded `unshare` +
      `pgrep`/`kill` from outside the namespace. `reboot(2)` itself
      returned EPERM in this sandbox (it needs `CAP_SYS_BOOT`, which
      this environment's user namespaces don't grant — same class of
      restriction as the mount EPERMs above, not a bug in the calls
      themselves), so the specific namespace-termination-by-signal
      behavior reboot(2) is documented to have (the parent's `wait()`
      seeing the child die by SIGHUP for restart / SIGINT for power off)
      is unverified here; what's confirmed is that the syscall is
      attempted correctly and failure is handled gracefully rather than
      hanging or panicking.
      Known minor gap: the shutdown-flag check and `waitpid` in the reap
      loop aren't atomic with each other, so a `respawn` service can in
      principle be restarted once more in the narrow window between a
      shutdown signal arriving and the next loop iteration noticing it
      (observed once during testing: a respawned process, started right
      before shutdown was noticed, got reaped correctly on the very next
      iteration — the system still converges to a clean shutdown, just
      with a possible one-extra-restart race rather than a hard
      guarantee of zero).
      Not implemented: real reboot/poweroff *orchestration* beyond the
      syscall itself (syncing and unmounting filesystems first), and any
      ordering more expressive than "block until this one line finishes"
      (no dependency graph, no "start B only after A is *ready*" for
      long-running services).

### Phase 4 — shell & base-devel toolchain (started)
bash replacement, makepkg equivalent, build tooling.

- [x] `sh`/`bash` — vendored `brush-shell` (a real POSIX/bash-compatible
      shell implementation in Rust — reimplementing a shell's parser,
      expansion rules, and job control from scratch would dwarf every
      other utility in this repo combined) into `coreutils-rs`'s
      multicall dispatch. `brush_shell::entry::run()` doesn't fit the
      pattern every other utility here follows: it reads real process
      argv itself instead of taking an args iterator we hand it, and it
      calls `process::exit()` internally rather than returning a code.
      That's exactly right for the symlink form (`bash -> coreutils-rs`,
      where real argv already looks like a normal shell invocation) but
      breaks the `coreutils-rs bash args...` convenience form, where
      argv[0] would be `coreutils-rs` and argv[1] `bash` — brush would
      try to parse "bash" as a script file. Fixed generally (not just
      for brush) by re-executing ourselves with a corrected `argv[0]`
      via `Command::arg0` whenever that form names `sh`/`bash`.
      Verified against real bash for: arithmetic expansion, `for`
      loops, conditionals (`[ -f ... ]`), functions, command
      substitution, bash arrays (`arr=(a b c)`, `${arr[1]}`), bash case-
      conversion expansion (`${x^^}`), pipes to an external command
      (`grep`), and output redirection — all byte-identical. Also
      verified the re-exec fix directly: `coreutils-rs bash -c 'echo
      $0'` correctly prints `bash`, confirming argv[0] carries through.
      Not implemented/unverified: brush's own coverage gaps against
      real bash (there will be some, e.g. more obscure job-control or
      parameter-expansion edge cases) haven't been separately audited
      beyond the checks above — this leans on brush's own test suite
      for the rest of its compatibility claim rather than re-verifying
      it here.
- [x] `makepkg-rs` — a new crate building a real installable package
      (`.pkg.tar.zst`, matching real makepkg's own layout closely
      enough for `pacman-rs -U` to install it) from a PKGBUILD, the
      same shape as real makepkg. A PKGBUILD is a bash script, not a
      data format — there's nothing to "parse" without actually
      sourcing it in a real shell, so this hands it to a real bash to
      extract variables (`declare -p`) and function names (`declare
      -F`), then runs whichever of `prepare`/`build`/`check`/`package`
      are defined with the usual `$srcdir`/`$pkgdir`/`$startdir` env
      vars set. Prefers a sibling `coreutils-rs` binary (dogfooding
      this project's own `bash`) over the system one when the
      workspace is built together, falling back to system `bash`
      otherwise. `source=()` URLs are downloaded with `ureq` and
      verified against `sha256sums=()` (`"SKIP"` entries skip
      verification, matching real makepkg), with recognized archive
      formats (`.tar.gz`/`.tar.zst`/`.tar`) extracted into `src/`.
      Needed two additions to `alpm-rs` to close the loop:
      `parse_pkginfo`/`write_pkginfo` for the `.PKGINFO` metadata
      format makepkg embeds in every package (different from the
      local/sync db's own `desc` format despite describing the same
      package), and `pacman-rs` gained `-U` (install a local package
      file directly) to actually install what this builds — see
      Phase 1's checklist for `-U`'s own verification against a real
      Arch package.
      Verified end-to-end, twice: (1) a PKGBUILD with no external
      source, just a `package()` that writes a script — built, then
      installed with `pacman-rs -U` into a sandboxed root, then the
      *actually-installed* binary was executed and produced the
      expected output; (2) a PKGBUILD exercising the parts (1) didn't
      — a `source=()` entry served over a real local HTTP server (not
      mocked), `sha256sums` verification, `.tar.gz` extraction, and a
      `build()` function — confirmed downloading, checksumming,
      extracting, and building all happened correctly (`build()`
      logged running from the correct extracted-source directory),
      then installed and ran the result the same way as (1). Repeated
      the first check again using only the release binaries end to
      end (`makepkg-rs` → `pacman-rs -U` → run) to confirm the real
      deployment path, not just the debug build used while iterating.
      One real bug caught and fixed along the way: the first archive
      built didn't include directory entries (`usr/`, `usr/bin/`),
      only files, which `pacman-rs`'s extractor — like real pacman's —
      assumes already exist rather than creating missing parents
      itself; fixed by using `tar::Builder::append_dir_all` instead of
      a file-only walk.
      Known gaps: single-package PKGBUILDs only (no split `pkgname=()`
      packages), no `.install` scriptlets, no PGP source verification,
      no dependency resolution before building (real makepkg would
      refuse to build without `makedepends`/`depends` present; this
      doesn't check), and the `declare -p` output parser handles the
      common case (quoted scalars, indexed arrays) rather than being
      fully shell-quoting-aware.

## Non-goals
Rewriting every package in the Arch repos (tens of thousands of packages,
most already upstream projects in their own languages) is not a software
task — it's not being attempted. "Fully in Rust" here means: Rust userland
core + Linux kernel, same as how "Arch Linux" is glibc/bash/systemd +
Linux kernel today.
