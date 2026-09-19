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

## Real boot test (2026-09-18)

Every phase below was, until this point, verified component-by-component:
each tool checked against the real thing it replaces, in isolation, invoked
directly on the host. That leaves a real, distinct question unanswered: do
the pieces actually work *together*, assembled into a system, with
`archrs-init` as *genuine* PID 1 under a real kernel with real privileges —
not just inside an unprivileged `unshare` namespace, where mounting
`/proc`/`/sys`/`/dev` and the `reboot(2)` syscall had both returned EPERM
(documented as an unverified sandbox limitation in Phase 3's own section).

Closed that gap with an actual QEMU boot, entirely without host root
(no `sudo`, no loop-mount) — now a repeatable check via
`scripts/boot-test.sh` (needs `cargo build --release` first; `--clean`
forces a fresh base-system install, otherwise the cached rootfs makes a
re-run take single-digit seconds), not just a one-off manual test: it
builds the same setup below, boots it, and greps the serial log for the
specific PASS markers this section describes, exiting non-zero if any are
missing. `pacman-rs -S` installed a real minimal base
system (`glibc`, `filesystem`, `bash`, plus `xz`/`file` for `coreutils-rs`'s
own runtime library needs) into a sandboxed root against real Manjaro
mirrors; `mke2fs -d <dir>` (populates an ext4 image directly from a host
directory, no mount required) turned that into a bootable disk image;
`archrs-init` replaced the real `bash`/coreutils with `coreutils-rs`
(symlinked for all ~117 utilities it dispatches) as `/sbin/init.archrs`;
QEMU booted the *host's own kernel* (`-kernel /boot/vmlinuz-...`) against
that disk image over virtio-blk, with serial console output.

Confirmed, for real, everything the sandbox couldn't show:
- `archrs-init` mounted `/proc` and `/sys` for real, as genuine PID 1 with
  genuine root — the exact operation that returned EPERM under `unshare`.
  `/dev` returned `EBUSY` instead of succeeding, because this kernel
  auto-mounts `devtmpfs` on `/dev` before init even runs — a real boot
  behavior the unprivileged test had no way to surface, since it never got
  past its own EPERM. Fixed by treating `EBUSY` here as "already mounted,
  fine" instead of printing a spurious error — a real bug caught only by
  booting for real, not something inferable from the sandbox test alone.
- `coreutils-rs`'s `bash` (brush) ran a real, non-trivial shell script as
  the actual init-spawned session: command substitution, arithmetic
  (`$((6*7))`), file I/O, calling into other `coreutils-rs`-dispatched
  utilities (`awk`, `ps`, `free`, `cat`, `kill`) — as PID 1's actual child,
  not a synthetic standalone invocation.
- `ps aux` (this project's own `procfs`-based implementation) correctly
  listed the *real* process tree a genuine kernel boot produces — every
  kernel thread (`kthreadd`, `kworker/*`, `ksoftirqd`, `rcu_preempt`, etc.),
  not just the handful of processes an `unshare` sandbox has.
  `free`/`awk` output was equally correct against real boot-time data.
- SIGUSR2 sent to the real PID 1 (`kill -USR2 1` from inside the running
  script) correctly triggered the shutdown sequence *and the real
  `reboot(2)` syscall itself succeeded* — the kernel's own log confirms it
  (`reboot: Power down`) — closing the other EPERM gap from the `unshare`
  test, where reboot(2) never actually completed.
- Re-run after adding `diff`/`cmp`/`dmesg`/`chroot`/`hostname`
  (Phase 5): `diff` correctly diffed two real files written by the
  init-spawned script; `dmesg` — which needs real
  `CAP_SYSLOG`/`CAP_SYS_ADMIN` to open `/dev/kmsg` at all, gated by
  `kernel.dmesg_restrict` independently of the file's own permission
  bits — read and correctly formatted genuine kernel ring-buffer
  messages (`[    0.000000] Linux version ...`); `chroot` made a real
  `chroot(2)` syscall succeed (needs real `CAP_SYS_CHROOT`) and the
  chrooted process's own `hostname` correctly read back the kernel's
  UTS-namespace hostname. Three more real capabilities the `unshare`
  sandbox has no way to grant or verify at all.

This is real evidence archrs can be a system's actual boot init and
userland, not just a set of individually-correct binaries — but it's one
boot of one minimal script-driven session, not a general "it's production
ready" claim. Still open, deliberately not addressed by this test: real
multi-user service management (archrs-init's config format is not
systemd-unit-compatible — a permanent architectural difference, not a gap
to close), `makepkg-rs` against a real-world AUR/official PKGBUILD (only
synthetic test PKGBUILDs have been tried), and anything above the base
userland (device management, networking daemons, a display stack).

## Phases

### Phase 1 — `alpm-rs` / `pacman-rs` (functionally complete)
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
- [x] Utilities outside real coreutils that a base install still needs
      (grep, sed, find, tar, gzip, less, ...) come from separate GNU
      projects, not `uutils/coreutils` — each needed its own sourcing
      decision instead of a ready `uu_*` crate:
  - [x] `find`, `xargs`, `locate`, `updatedb` — vendored from the
        `findutils` crate, GNU findutils' own official uutils Rust port
        (same organization as `uutils/coreutils`, published the same
        way). Verified `find -name`/`-type` and `xargs` byte-identical
        against real GNU findutils.
  - [x] `tar` — GNU tar has no Rust port to vendor, so this is our own
        thin CLI (`src/tar_cmd.rs`) over the `tar`/`flate2`/`zstd`/
        `bzip2`/`xz2` crates (the first three already used for real
        package archives in `alpm-rs`; bzip2/xz support added in Phase
        5, see below). Covers `-c`/`-x`/`-t` with gzip/bzip2/xz/zstd
        compression, both the old bundled-flag style (`tar xvf a.tar`)
        and normal `-xvf`. Not implemented: incremental archives,
        extracting a subset of named members.
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

### Phase 3 — init & service management (functionally complete for its scoped goal — a minimal live-image init, not a systemd replacement)
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
      Update: mounting `/proc`/`/sys`/`/dev` — unverifiable under
      `unshare` (EPERM for all three there) — was subsequently confirmed
      for real under an actual QEMU boot with `archrs-init` as genuine
      PID 1; see "Real boot test" above. `/proc` and `/sys` mounted
      cleanly; `/dev` returned `EBUSY` because this kernel auto-mounts
      `devtmpfs` there before init runs, which the code now treats as
      success rather than printing a spurious error (a real, if minor,
      bug the `unshare` test had no way to surface).
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
      `pgrep`/`kill` from outside the namespace. Under `unshare`,
      `reboot(2)` itself returned EPERM (needs `CAP_SYS_BOOT`, which
      that environment's user namespaces don't grant), so only the
      syscall attempt and graceful-failure handling were confirmed
      there. Update: subsequently verified for real under an actual
      QEMU boot — `SIGUSR2` sent to the genuine PID 1 triggered
      `reboot(RB_POWER_OFF)`, and it *succeeded*: the kernel's own log
      shows `reboot: Power down` immediately after. See "Real boot
      test" above.
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

### Phase 4 — shell & base-devel toolchain (functionally complete)
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
- [x] `package()` runs under fakeroot, matching real makepkg. Vendored
      `pseudoroot` (a real Rust fakeroot via `LD_PRELOAD` library
      interposition — not something to hand-roll) rather than requiring
      actual root or `sudo`. Verified for real: a `chown root:root`
      call inside `package()` succeeds and `id` reports `uid=0` from
      *inside* that fakeroot session, while a plain `ls`/`stat` run
      *outside* it (a separate, unwrapped process) still shows the
      real invoking user — confirming it's genuinely a same-process-
      only illusion via interposition, not an actual privilege change,
      exactly how real fakeroot works.
      Caught a second real bug this surfaced: fakeroot only fakes what
      the *wrapped subprocess's own* syscalls see. `package_archive`'s
      tar-writing code runs in-process (no subprocess to wrap), so it
      was reading genuine filesystem ownership (the build user, not
      root) when writing tar headers — real makepkg avoids this by
      building the tar itself from *inside* the same fakeroot session,
      which isn't an option here. Fixed by forcing every tar entry's
      uid/gid to 0/0 unconditionally at packaging time instead, which
      matches fakeroot's own default behavior for the overwhelming
      majority of real packages (ones that never explicitly chown to
      some other uid) — confirmed the built archive now shows
      `root/root` for every entry instead of the build user's real
      uid/gid, with no regression to either previously-verified
      end-to-end case.
      Known gap: a PKGBUILD that deliberately assigns non-root
      ownership to specific files (rare, e.g. a setgid directory) would
      be recorded as root anyway, since forcing 0/0 doesn't distinguish
      that case from the common one.
      Known gaps: single-package PKGBUILDs only (no split `pkgname=()`
      packages), no `.install` scriptlets, no PGP source verification,
      no dependency resolution before building (real makepkg would
      refuse to build without `makedepends`/`depends` present; this
      doesn't check), and the `declare -p` output parser handles the
      common case (quoted scalars, indexed arrays) rather than being
      fully shell-quoting-aware.

**Hardened against real AUR packages (2026-09-19).** Every prior
`makepkg-rs` check above used synthetic, hand-written test PKGBUILDs —
a real, distinct gap, since only a real PKGBUILD exercises the actual
variety (checksum algorithms, archive formats, multi-file git repos)
real upstream maintainers use. Cloned two real AUR packages via `git
clone` (the way an actual user gets one, pulling in every auxiliary
file, not just the PKGBUILD) and built each with `makepkg-rs`, then
installed the result with `pacman-rs -U` and ran the built binary:
- `tty-clock` — real `prepare()` applying a real patch file, `build()`
  running `make`, `package()` using `install -D`, and two local
  (non-URL) auxiliary source files (a patch, a LICENSE) that only
  exist in the package's own git repo. Caught a real, security-
  relevant gap immediately: this PKGBUILD uses `b2sums` (BLAKE2b-512,
  makepkg's own modern default), which this tool didn't recognize at
  all — checksum verification was silently skipped entirely rather
  than erroring, meaning a corrupted or tampered download would have
  gone completely unnoticed.
- `cbonsai` — a GitLab-hosted `.zip` source (`sha256sums`, not
  `b2sums`) with no separate `build()`, doing real compilation
  *inside* `package()` via `make ... install`. Caught a second real
  gap: `.zip` wasn't a recognized archive format at all (only
  `.tar`/`.tar.gz`/`.tar.zst` were), which would have failed outright
  on any GitLab-archive-sourced package — a real, common shape, not a
  corner case, since GitLab's own archive URLs default to zip where
  GitHub's default to tarballs.

Both real fixes:
1. Added `alpm_rs::verify::ChecksumKind` (`Blake2b`/`Sha512`, alongside
   the existing `Sha256`) via the `blake2` crate, matched against
   known-vector unit tests the same way `sha256_hex` already was.
   `makepkg-rs` now checks `b2sums`/`sha512sums`/`sha256sums` (whichever
   are present, all of them if more than one is), and — this is the
   real hardening, not just added coverage — now **refuses to build at
   all** when a remote source has no checksum entry this tool can
   verify (an entry using an unsupported algorithm like `md5sums`
   counts as none), rather than silently proceeding unverified the way
   the original single-algorithm check did. `SKIP` entries are still
   honored, matching real makepkg's own convention. Verified the
   refusal directly with a synthetic PKGBUILD carrying only an
   `md5sums` entry (correctly rejected) and confirmed `SKIP` still
   builds normally.
2. Vendored the `zip` crate (2.x) for `.zip` extraction, alongside the
   existing tar-based formats.
Both real packages built, installed via `pacman-rs -U` into a real
root, and ran successfully afterward (`tty-clock`'s real ncurses
display rendered the actual current date; `cbonsai`'s compiled binary
installed as a genuine, correctly linked ELF executable) — the same
two-stage verification (build, then actually install and run) already
established for the synthetic-PKGBUILD checks above.

**Extended to real VCS (`-git`) sources (2026-09-19).** One of the
most common real AUR shapes hadn't been tried yet: a `-git` package
whose `source=()` is a live git repository rather than a downloadable
archive, with a `pkgver()` function computing the real version from
`git describe` after checkout — `makepkg-rs` had no VCS handling at
all before this (a `git+https://...` URL would have been handed
straight to the plain-HTTP downloader and failed outright). Tried two
real, structurally different `-git` PKGBUILDs before settling on a
clean one: `slock-git`/`st-git` both reference `$BUILDDIR`, a real but
optional makepkg.conf setting most users never set, in a way that
breaks even under real makepkg when it's unset (`cp file "$BUILDDIR"`
with `BUILDDIR` empty) — a real upstream PKGBUILD assumption mismatch,
not a gap in this tool, so not chased. `dmenu-git` has no such
dependency and is otherwise a clean, representative example (real
`git+https://` source, `pkgver()`, real `prepare()`/`build()`/
`package()`).
- [x] VCS `source=()` entries — recognizes `git+<url>` (the standard
      makepkg VCS-prefix syntax) and a bare `git://` URL (used
      directly by some upstream PKGBUILDs, e.g. suckless's own,
      unambiguous without the prefix since the scheme alone says
      "git"), including the `#tag=`/`#branch=`/`#commit=` pinning
      syntax real makepkg supports. Clones via the real `git` binary
      (not a vendored git implementation — same reasoning as shelling
      out to `gpg` for OpenPGP or `patch`/`make`/`gcc` for building:
      reimplementing git wire protocol handling isn't a reasonable
      trade for what this needs) into `$srcdir/<reponame>`, named the
      same way real makepkg does — the URL's own last path segment, or
      an explicit `name::` prefix when the source entry has one.
      Re-running against an already-cloned checkout skips re-cloning
      (this project's own idempotent-rebuild convention, matching how
      `boot-test.sh`'s cached rootfs works) rather than always
      re-fetching.
      `pkgver()`, if defined, now runs (with `$srcdir` as its working
      directory, matching every real example found) right after
      sources are prepared and before `prepare()`/`build()` — its
      output becomes the real package version for the rest of the
      build and the final output filename, overriding the PKGBUILD's
      own placeholder `pkgver=` value the same way real makepkg does.
      Verified for real against `dmenu-git`: cloned the actual live
      `git.suckless.org/dmenu` repository, correctly computed a real
      current version (`5.4.r6.61e0072`) that differs from the AUR
      PKGBUILD's own stale placeholder (`5.2.r7.7ab0cb5`) — proving
      this is genuinely running `git describe` against a live clone,
      not just echoing the static value — then built, installed via
      `pacman-rs -U`, and confirmed the installed binary is a real,
      correctly linked ELF executable.
      Not implemented: `svn+`/`hg+`/`bzr+` sources (real but rarer than
      git in practice), `noextract`, VCS-source-specific `makedepends`
      auto-detection.

**Extended to real split packages, and caught a real symlink-handling
bug (2026-09-19).** `pkgname=()` split packages (one PKGBUILD, several
output `.pkg.tar.zst` archives, each via its own `package_<name>()`)
are common for font families, plugin suites, and anything shipping a
runtime + headers/docs as separate installable units — a real,
documented gap until now. Found a real split-package AUR example by
downloading AUR's own package metadata dump and grouping by
`PackageBase` (`font-symbola` looked ideal but needs `fontforge`, not
installed here and not something to chase down mid-test; `ttf-readex-
pro` needed nothing beyond what was already available and is otherwise
representative: a real GitHub-tarball source, one shared `prepare()`,
two `package_<name>()` functions).
- [x] Split packages — `pkgname` with more than one entry now runs
      `prepare`/`build`/`check` once (shared, matching real makepkg),
      then each `package_<name>()` into its own `pkg-<name>/`
      directory, producing one archive per name. Per-sub-package
      metadata overrides (`pkgdesc+=`, `depends=`, etc., reassigned
      inside `package_<name>()`) are captured the same way real
      makepkg does it: read back those variables' final values right
      after the function runs, in the same bash invocation, falling
      back to the shared top-level value for anything a given
      sub-package doesn't touch.
      Verified for real against `ttf-readex-pro`: both `ttf-readex-
      pro` and `ttf-readex-pro-variable` built as separate, correctly
      distinct archives (right `pkgname`/`pkgdesc` in each `.PKGINFO`,
      shared `pkgbase`, correct per-package file sets and sizes), and
      both installed successfully side by side via `pacman-rs -U`.
- [x] **Symlink handling in `package_archive`** — a real, previously
      undiscovered bug, caught only because `ttf-readex-pro` happens
      to `ln -s` a shared fontconfig file into place (no earlier test
      package created a symlink in its `pkgdir`). `walk_all` classified
      filesystem entries by calling `path.is_dir()`/implicitly
      "else file", but that check *follows* symlinks — a symlink to a
      regular file was misfiled as `Entry::File`, whose writer then
      opened it with a link-following `File::open` (reading the
      *target's* content) while the tar header still carried the
      original entry's `lstat`-based size (0 bytes, since a symlink
      itself has no content) — a size/content-length mismatch that
      corrupted the archive (confirmed directly: `tar: Skipping to
      next header` on extraction, gone after the fix). Fixed by
      checking `fs::symlink_metadata` (`lstat`, never follows) first,
      adding a real `Entry::Symlink` case, and writing it with `tar`'s
      own `append_link` (entry type `Symlink`, real link target via
      `fs::read_link`, zero content) instead of trying to write file
      bytes at all. Verified: both `ttf-readex-pro` archives now
      extract cleanly with a correctly-typed `lrwxrwxrwx` entry, and
      installing them creates a real, correctly resolving symlink on
      disk. Re-verified no regression on every previously-working real
      package (`dmenu-git`, none of which happen to contain a symlink,
      so this was purely additive).
- [x] `which` and `patch` — the remaining small, well-scoped
      base-devel-adjacent utilities PKGBUILDs commonly need. `which`
      vendors the `which` crate (real cross-platform `PATH` lookup);
      verified against `/usr/bin/which` directly (this environment's
      interactive-shell `which` is itself a shell builtin that reports
      aliases, an unrelated wrinkle like the earlier `ugrep` one — not
      a discrepancy in ours). `patch` vendors `patch-apply` (a real
      unified-diff parser/applier) for `-p<N>`/`-i`/stdin.
      Caught and worked around a real bug in the vendored crate: its
      `apply()` always drops the file's trailing newline regardless of
      whether the original had one (it rejoins split lines with
      `Vec::join("\n")`, which can't represent a final newline).
      Verified against real GNU `patch` applying the same unified diff
      to the same file — output was byte-identical only after adding a
      one-line fix that restores the trailing newline when the
      original had one; documented as leaning on this workaround rather
      than a corrected upstream crate.
      Not implemented: context/normal diff formats, fuzzy matching for
      drifted line numbers, `-R` (reverse), `which -s` (silent).

### Phase 5 — additional userland coverage (complete)
All four originally-planned phases are functionally complete (see above).
This phase was what came after: closing more real day-to-day gaps in
`coreutils-rs`, following the same standard set throughout this project
— vendor a real crate when one exists rather than hand-roll, verify
against the real tool being replaced, document what's out of scope —
rather than an exhaustive, fixed checklist the way Phases 1-4 were. See
the closing note at the end of this section for where the deliberate
scope boundary landed.

- [x] `awk` — AWK is its own full programming language (patterns,
      actions, control flow, user functions, associative arrays), so
      this vendors `awk-rs` (a from-scratch AWK lexer/parser/
      interpreter aiming for POSIX + gawk-extension compatibility)
      rather than hand-rolling an interpreter. Its CLI logic lives only
      in its own `main.rs`, not exposed as a callable library entry
      point, so `awk_cmd.rs` re-implements that argument-parsing layer
      over its public `Lexer`/`Parser`/`Interpreter` types.
      Verified byte-identical against real GNU Awk 5.4.1 across: field
      printing, `NF`, pattern-matched conditions, `BEGIN`/`END` with an
      accumulator, `-F`, `-v`, stdin input, associative arrays with
      `for...in`, `printf` field-width/precision formatting, string
      functions (`length`/`substr`/`toupper`), user-defined functions,
      and `gsub`.
      Not separately audited beyond the above: the rest of `awk-rs`'s
      own POSIX/gawk coverage claim (see its own README) — this is a
      new, less battle-tested crate (0.2.0) compared to e.g. the
      `uu_*`/`grep-*`/`findutils` crates this project otherwise leans
      on, worth keeping in mind if something obscure misbehaves later.
- [x] `curl` — no real curl-CLI-compatible crate exists to vendor
      wholesale without pulling in a lot of unrelated scope (the
      closest candidate bundles BitTorrent and SSH support), so this
      is a thin CLI layer (`curl_cmd.rs`) over `ureq`, the same HTTP
      client already proven elsewhere in this workspace (`alpm-rs`
      fetching real package archives, `makepkg-rs` downloading
      PKGBUILD sources) — not a from-scratch HTTP client.
      Covers GET (default) / `-X` / implicit POST via `-d`, `-o`/`-O`,
      `-I` (headers), `-H`, `-A`, `-f` (fail on HTTP error status).
      `-L` is accepted as a no-op since `ureq` already follows
      redirects by default.
      Verified byte-identical against real `curl` for a plain GET and
      `-o` against a real local HTTP server (not mocked), `-O`
      correctly deriving the remote filename and downloading real
      content, `-I` returning real response headers, and a real HTTPS
      GET against an actual Arch mirror
      (`https://geo.mirror.pkgbuild.com/lastsync`) matching real curl
      byte-for-byte.
      Not implemented: `--data-urlencode`, cookies, `.netrc`, HTTP/2,
      client certificates.
- [x] `tar`: bzip2 (`-j`) and xz (`-J`) compression, completing the
      archive-format coverage real PKGBUILDs actually use (alongside
      gzip/zstd from Phase 2) — closes a real gap `makepkg-rs`'s own
      source extraction had. Vendors `bzip2`/`xz2` (bindings to the
      system `libbz2`/`liblzma`), the same pragmatic choice as `zstd`
      binding real `libzstd` elsewhere in this project, rather than a
      pure-Rust reimplementation of either format.
      Verified full bidirectional interop against real GNU tar for
      both formats: an archive built by ours extracts correctly with
      real tar, and an archive built by real tar extracts correctly
      with ours (byte-identical file trees both ways), plus
      extension-based auto-detection (`.tar.xz`/`.tar.bz2`) creating
      the right format without an explicit `-J`/`-j` flag.
- [x] `crond`/`crontab` (new crate, `archrs-cron`) — a cron daemon and
      crontab-management CLI, built on `crontab-rs`'s cron-expression
      parser/matcher (`Schedule`/`Crontab` — the fiddly, error-prone
      part: day-of-month/day-of-week OR semantics, ranges, steps,
      `@reboot`, etc.) rather than hand-rolling a cron expression
      evaluator.
      Deliberately scoped to **single-user cron**, not a full system
      cron replacement — a real, explicit security decision, not an
      oversight: multi-user cron (`/var/spool/cron/<user>` per-user
      tables, `/etc/crontab`/`cron.d` system tables with a `user`
      column) needs setuid/setgid privilege-dropping to run each job
      as its owning user, and `crontab-rs` itself only exposes its
      parsing/scheduling engine as *stable* public API — its own
      daemon/privilege-drop/mail-delivery modules are explicitly
      marked "not covered by semver," i.e. internal detail the crate
      author doesn't intend for reuse. Reimplementing that
      security-critical logic ourselves, on unstable internals,
      without the scrutiny it deserves, isn't a reasonable trade for
      what a personal automation tool needs — same reasoning as
      Phase 1 shelling out to `gpg` rather than reimplementing OpenPGP.
      `crond` reads one crontab (`~/.config/archrs/crontab` by
      default) and runs every matched job as whichever user `crond`
      itself runs as. `crontab` manages that one file: `-l`/`-r`/`-e`
      (edit via `$EDITOR`/`$VISUAL`, re-validated before being
      installed — a rejected edit leaves the previous crontab
      untouched) and installing from a file or stdin. Every install
      path validates with `Crontab::parse` first and refuses to save
      anything invalid.
      Verified for real: installed a `* * * * *` job via `crontab -`,
      confirmed `crontab -l` shows it, confirmed an invalid crontab is
      rejected without disturbing the valid one already installed, and
      ran `crond` live for ~65 real seconds — it correctly fired the
      job at both one-minute boundaries crossed and appended the
      expected output both times (checked via a real timestamped log
      file, not mocked). Also caught and fixed a real dispatch bug
      during this testing: `crontab -` (read from stdin) was being
      rejected as an unrecognized flag because `"-"` starts with `-`,
      same shape of bug as `-e`/`-x` bundle-vs-flag ambiguity elsewhere
      in this project.
      Not implemented: `-u` (other users' crontabs — the exact
      multi-user surface this scope decision stays out of), mailing
      job output, and `RANDOM_DELAY`/`CRON_TZ` handling beyond what
      parsing validates. `@yearly`/`@daily`/`@hourly` etc. shortcuts
      parse and validate correctly (confirmed) and fire on schedule
      the same as any other entry, since `Schedule::matches` handles
      them internally — but `@reboot` (meant to fire once when the
      daemon starts) never does, since `crond`'s loop only checks
      entries against the current time and has no separate
      run-once-at-startup path for it.
- [x] `ps`/`free`/`uptime` — `procps-ng`, a separate upstream project
      from `uutils/coreutils`, so no `uu_*` crate exists for any of
      these. Vendors `procfs` (a real `/proc` parser, already used
      transitively elsewhere in this dependency tree) and `users`
      (uid-to-username lookup) rather than hand-parsing `/proc`
      ourselves (`src/procps_cmd.rs`).
      `free` ended up the most rigorously verified of the three: got
      byte-identical against the real binary (`-k`/`-m`/`-g` and the
      default) only after two real formula corrections, both caught by
      diffing rather than assumed correct:
      1. Naive `used = total - free - buffers - cached` didn't match;
         subtracting reclaimable slab too got `buff/cache` exactly
         right but `used` still didn't match.
      2. Turned out modern `free` doesn't derive `used` from the
         buffers/cache breakdown at all — it's simply
         `total - available`. Once that clicked, `-k`/`-m`/`-g`, the
         default, and even the exact column widths (measured
         character-by-character from real `free`'s own output rather
         than guessed) all matched byte-for-byte.
      `-h` (human-readable) is close but not exact — a real, minor,
      documented cosmetic gap in decimal-rounding rules, not chased
      further once the data-bearing modes were exact.
      `ps aux` was verified by comparing the PID set against real
      `ps aux` (matching aside from each invocation's own transient
      `awk`/`sort` pipeline subprocesses, expected) rather than an
      exact line diff, since procps-ng's own column formatting has
      version-specific quirks not worth chasing byte-for-byte; `%CPU`
      is not implemented (always prints `0.0`) since computing it
      properly needs either two time-separated samples or precise
      boot-time math that this scope doesn't justify — documented
      rather than silently wrong. `uptime`'s duration and load
      averages matched real `uptime` exactly; its leading
      current-time/logged-in-users prefix isn't implemented.
      Caught and fixed a real, more broadly applicable bug while
      testing `ps aux | head`: `coreutils-rs`'s own dispatch targets
      (tar/gzip/grep/sed/awk/curl/ps/... — anything not going through
      a `#[uucore::main]`-generated utility, which already handles
      this internally) never restored SIGPIPE to its default
      disposition, so piping any of them into something that closes
      the pipe early (`| head`) panicked instead of exiting quietly
      like every other Unix CLI. Fixed once, generally, at the top of
      `coreutils-rs`'s own `main()` — the same fix `pacman-rs` already
      had for the same reason.
- [x] `file` — vendors the `magic` crate, real FFI bindings to the
      system's actual `libmagic` C library and its full default magic
      database, rather than a "safe Rust re-implementation of
      libmagic" alternative that exists too — binding the real library
      gets the exact same detection results real `file` produces, not
      a second, independently-maintained copy of the magic database
      that could drift from it (same reasoning as `zstd`/`bzip2`/`xz2`
      elsewhere in this project). Covers `-i`/`--mime` and
      `-b`/`--brief`.
      Verified byte-identical against real `file` across plain text,
      a shell script (detected as `POSIX shell script, ASCII text
      executable`), a real ELF binary (full detail string including
      interpreter path and BuildID matched exactly), an HTML file, and
      `-i` MIME-type output.
      Not implemented: stdin (`-`), directory recursion, `--extension`.
- [x] `ping` — vendors the `ping` crate for real ICMP echo request/
      reply packet construction and matching (not something to
      hand-roll on raw sockets), with our own CLI/loop around it.
      Uses an unprivileged `DGRAM` ICMP socket (the crate's Linux
      default) — no root/`CAP_NET_RAW` needed as long as
      `net.ipv4.ping_group_range` permits it, the same mechanism real
      `ping` uses to work unprivileged on modern Linux (confirmed
      enabled system-wide on this machine before relying on it).
      Real, deliberate behavior differences from real `ping`, not
      bugs: no `ttl=` field (DGRAM sockets on Linux never see the
      reply's IP header — the crate's own documented limitation), and
      `-c COUNT` is *required* rather than optional, since real
      ping's own default (run until Ctrl-C) is a poor fit for a
      non-interactive dispatched command.
      Verified for real, not just against localhost: `ping -c 3
      127.0.0.1` got real replies with correct source/RTT; `ping -c 2
      8.8.8.8` resolved and reached a real external host with
      plausible real-world RTTs (~18-22ms); an unreachable address
      (`192.0.2.1`, a documentation/test-only range that's never
      routable) correctly reported 100% packet loss and a non-zero
      exit code.
      Not implemented: IPv6-specific flags, `-f`/`-A`
      (flood/adaptive), `-s` (payload size).
- [x] `column` — from util-linux, a separate project from both
      `uutils/coreutils` and GNU. A genuinely simple text-alignment
      algorithm with no real "engine" worth vendoring (unlike most of
      what else is in this crate), so this is a plain from-scratch
      implementation. Covers `-t` (table mode, matching real
      `column -t`'s exact spacing: per-column field width, left-
      justified, joined by two literal spaces, last field on each line
      left unpadded) and `-s`/`-s<SEP>` (custom input separator).
      Verified byte-identical against real `column` for default
      whitespace-separated input, a custom `-s:` separator, and stdin.
      Explored but deliberately declined: `dig`/DNS lookup — the only
      real Rust DNS resolver library (`hickory-resolver`) defaults to
      async/tokio, which would be the first async dependency in this
      otherwise fully synchronous codebase for one command (~139
      transitive packages). Flagged to the user as a real architectural
      call rather than decided unilaterally; they chose to skip it.
      `curl`/`ping` already cover the common hostname-resolution needs
      that would otherwise motivate it.
      Not implemented: `-o` (custom output separator), `-c` (output
      width / multi-column list mode for non-table input), `-J`/`-N`
      (JSON/named-column modes).
- [x] `diff`/`cmp` — GNU diffutils, a separate upstream project from
      `uutils/coreutils`, but with its own official uutils-maintained
      Rust port to vendor: the `diffutils` crate (library name
      `diffutilslib`). Its multicall CLI dispatch (by argv[0], picking
      `diff` vs `cmp`) isn't part of the published library though —
      only `src/lib.rs`'s `pub mod`s are, which omits the `diff`
      module's own CLI glue entirely (it exists only in the binary
      crate's own `main.rs`, never `pub`). Same situation as `awk-rs`
      needing its own CLI layer in `awk_cmd.rs`: `diff_cmd.rs`
      re-derives that thin (~40-line) CLI wrapper itself, calling
      straight into the vendored crate's real, unmodified, public
      diff-algorithm and flag-parsing functions for everything that
      matters (`{normal,unified,context,ed,side}_diff::diff`,
      `params::parse_params`). `cmp` didn't even need that: its own
      `cmp()` function already does 100% of its real output (default
      "differ" message, `-l` verbose byte listing, EOF messages) as a
      side effect before returning just a summary
      `Cmp::Equal`/`Cmp::Different`, so `run_cmp` is a straight,
      unmodified call into the vendored crate's own public logic.
      Verified byte-identical against real GNU diffutils 3.12 across:
      `diff -u`/`-c`/normal/`-e` on files with real differences,
      `-q`/`--brief`, `-s` on identical files, reading one side from
      stdin (`-`); `cmp` on identical files (silent, exit 0),
      differing files (default message + exit 1), and `-l` (verbose
      byte-by-byte octal listing) — all byte-for-byte matches.
      Two real, minor formatting gaps in the vendored crate (0.5.0,
      a newer, less battle-tested port than `uu_*`/`findutils`),
      caught by diffing against real diffutils and confirmed
      functionally harmless rather than patched around, the same
      standard applied to `free -h` and `ps aux`'s `%CPU` elsewhere in
      this phase:
      1. Default (normal) format prints a redundant `,N` on
         single-line append/change ranges (`3a4,4` instead of real
         diff's `3a4`) — confirmed real `patch` accepts both forms
         identically (tested: applying our output with real `patch`
         reconstructed the target file correctly).
      2. `-e` (ed script) emits hunks top-to-bottom with line numbers
         valid against the *progressively-updated* buffer, instead of
         GNU diff's convention of bottom-to-top hunks numbered against
         the *original* file. Different convention, not a correctness
         bug: confirmed by simulating sequential command application
         (no real `ed` binary available in this environment) that our
         script still reconstructs the target file exactly when run
         top-to-bottom, which is what actually piping it into `ed`
         does.
      `cmp`'s EOF message also quotes the filename with plain ASCII
      apostrophes where real `cmp` uses typographic (curly) quote
      marks — a one-character cosmetic difference in the vendored
      crate's own error text, not chased further.
      Not implemented: `diff -r` (recursive directory comparison —
      `parse_params` has no notion of directories, only two file
      paths) and `diff3`/`sdiff` (present in real diffutils but not
      vendored here; not common PKGBUILD/day-to-day needs).
- [x] `dmesg` (util-linux) — no crate to vendor here, and none needed:
      the real "engine" is the kernel's own structured `/dev/kmsg`
      record interface (documented in
      `Documentation/ABI/testing/dev-kmsg`), simple enough to read
      directly (each `read()` returns exactly one
      `<facility*8+level>,seq,timestamp_usec,flags;message` record) —
      same reasoning as `column` needing no vendor target. Opened
      non-blocking so reads stop at the currently buffered messages
      (`EAGAIN`) instead of following forever, matching real `dmesg`'s
      non-`--follow` default.
      Reading `/dev/kmsg` needs real `CAP_SYSLOG`/`CAP_SYS_ADMIN`,
      gated by the `kernel.dmesg_restrict` sysctl independently of the
      device's own file permissions — confirmed real `dmesg` fails
      identically unprivileged on this dev machine (`dmesg_restrict=1`
      here), with the exact same wording this wraps (`dmesg: read
      kernel buffer failed: Operation not permitted` — matched
      precisely by using `libc::strerror` instead of `io::Error`'s own
      `Display`, which appends a Rust-specific `(os error N)` suffix
      real `dmesg` doesn't have).
      Verified for real as root, the only way to verify this at all:
      the real boot test (see above) now reads and correctly formats
      genuine kernel ring-buffer messages from an actual boot
      (`[    0.000000] Linux version ...`, monotonic timestamps
      matching real dmesg's default format exactly).
      Not implemented: `-T`/`--ctime` (wall-clock timestamps), `-l`/
      `-f` (level/facility filtering), `-c` (clear-after-read),
      `--follow`, colorized/JSON output.
- [x] `chroot`, `hostname` — two more real, official `uu_*` crates
      (`uu_chroot`, `uu_hostname`) that simply hadn't been picked up
      yet, same as every other coreutils entry in Phase 2 — no new
      pattern here.
      `hostname` verified byte-identical against the real binary.
      `chroot(2)` needs `CAP_SYS_CHROOT` (root, normally): confirmed
      real `chroot` fails identically unprivileged on this dev machine
      (`Operation not permitted`, exit 125 both — wording differs
      slightly, `uu_chroot`'s own message text, not patched); verified
      for real as root via the boot test, which now runs `chroot /
      hostname` and `chroot / true` from the init-spawned session —
      both a real `chroot(2)` syscall succeeding and the chrooted
      process's `hostname` correctly reading the (unchanged, since
      `chroot` only rebinds `/`) UTS namespace hostname.
- [x] `b2sum`, `basenc`, `pr`, `sha224sum`, `sha384sum`, `tac`,
      `truncate` — seven more official `uu_*` crates that had simply
      been missed when the rest of coreutils was vendored in Phase 2;
      no new pattern, just closing that gap. All seven verified
      byte-identical against the real binaries (`b2sum`, `basenc
      --base64`, `pr`, `sha224sum`, `sha384sum`, `tac`, and `truncate
      -s`).
- [x] `stty`, `more` — two more `uu_*` crates, both real, functioning
      implementations, but each with a real, visible fidelity gap
      against the traditional tool it replaces (younger corners of the
      uutils project than `uu_ls`/`uu_cat`-era crates) — found by
      testing against a real pty (`python3`'s `pty.fork()` +
      `TIOCSWINSZ`, since a plain redirected pipe reports `rows 0;
      columns 0` for both and hides the actual gaps) rather than a
      redirected-pipe diff:
      - `stty -a`'s termios flag listing omits a handful of legacy
        flags real GNU `stty` prints (`-iuclc`, `-ofill`, `-xcase`)
        and prints one it shouldn't (`-tandem`, a legacy alias GNU
        `stty -a` doesn't show). Everything else — speed, rows/
        columns, control characters, and the large majority of the
        termios flag table — matched exactly.
      - `more` renders as a full alternate-screen TUI pager (entering/
        leaving with `\e[?1049h`/`\e[?1049l`, matching how this
        crate's own `less`/`minus`-based `less_cmd.rs` behaves) with a
        `filename (N%)` status line, instead of traditional BSD/
        util-linux `more`'s simpler in-place `--More--(N%)` prompt
        with no alternate screen. Still a working, usable pager
        (confirmed paging and quitting both work over a real pty) —
        just a different, more `less`-like UI than classic `more`.
      Neither gap patched — both are real limitations of the vendored
      crates themselves, not something this thin dispatch layer
      introduced, and re-implementing either tool's rendering/flag
      table ourselves would mean forking a third-party crate rather
      than vendoring it. Documented rather than chased further, same
      standard as `free -h`'s rounding gap and `ps aux`'s `%CPU`.
- [x] `mount`/`umount` — real `mount(2)`/`umount2(2)` via the `nix`
      crate's `mount` module: the exact same crate and `MsFlags`
      bitflags `archrs-init` already uses for its own boot-time
      `/proc`/`/sys`/`/dev` mounts, so this is reusing an
      already-proven dependency rather than a new vendoring decision
      or a second hand-rolled `libc::mount` FFI call.
      `mount` with no arguments lists current mounts by reading
      `/proc/self/mounts` and reformatting each line as real `mount`'s
      default `source on target type fstype (options)` output —
      verified byte-identical against the real binary's listing.
      `mount SOURCE TARGET [-t FSTYPE] [-o OPTIONS]` translates the
      common comma-separated `-o` flags (`ro`/`rw`, `noexec`/`exec`,
      `nosuid`/`suid`, `nodev`/`dev`, `noatime`/`atime`/`relatime`,
      `sync`/`async`, `remount`, `bind`/`rbind`, `private`/`shared`/
      `slave`/`unbindable`) into `MsFlags`, passing anything else
      (e.g. tmpfs's `size=100m`) through verbatim as filesystem data —
      the same split real `mount` itself makes. `umount TARGET`
      (`-f`/`-l`) wraps `umount2`.
      Real mounts need `CAP_SYS_ADMIN`, so — same situation as
      `dmesg`/`chroot` — only genuinely verifiable as root: confirmed
      unprivileged failure matches real `mount`/`umount` (EPERM; exact
      wording differs, since real `mount` is a setuid binary doing its
      own pre-check with a friendlier message, while this calls
      `mount(2)` directly and reports the raw errno), then verified
      for real via the boot test, which now mounts a real `tmpfs` with
      `-o size=1m`, confirms it shows up correctly in `mount`'s own
      listing (`tmpfs on /mnt/tmpfstest type tmpfs (rw,relatime,
      size=1024k,...)`), writes and reads a file through it, and
      unmounts it cleanly — all as the init-spawned session's genuine
      root.
      Not implemented: `/etc/fstab` (`mount -a`, mount-by-label/UUID),
      loop-device auto-setup for mounting image files (real `mount`
      transparently `losetup`s these), `--bind`'s single-argument
      short form.

Phase 5 stops here as a deliberately-scoped milestone rather than
petering out mid-utility: every remaining official `uu_*` crate that
fits this project's existing vendor-and-wrap pattern has now been
picked up, `diff`/`cmp`/`dmesg`/`chroot`/`mount`/`umount` closed the
biggest remaining everyday-userland and system-administration gaps,
and the two hardest-to-verify additions (`dmesg`, `mount`, both needing
real root) are now permanent, automated regression checks in
`scripts/boot-test.sh` rather than one-off manual tests. What's
deliberately still outside this phase's scope, not from oversight:
- **Interactive/curses-heavy tools** (`top`, a real full-screen `vim`/
  `nano`) — meaningfully bigger projects in their own right, not a
  natural extension of the CLI-utility pattern used throughout.
- **Networking beyond `curl`/`ping`** (`ip`/`ifconfig`, `ss`/
  `netstat`) — real coverage needs netlink socket handling, a
  different problem shape than everything vendored so far.
- **Device/hardware enumeration** (`lsblk`, `lspci`, `lsusb`) — needs
  vendor/device ID databases (`pci.ids`/`usb.ids`) this project has no
  copy of and no clear source to vendor cleanly.
- **Privilege-management tools** (`su`, `sudo`, `passwd`,
  `useradd`/`usermod`) — deliberately out of scope for the same reason
  `crond`/`crontab` stayed single-user: reimplementing privilege-
  dropping and credential handling ourselves, without the security
  scrutiny that code deserves, isn't a reasonable trade.
- **Kernel module loading** (`insmod`/`rmmod`/`modprobe`) — not
  exercised by anything this project's own boot/package-management
  path actually needs yet.

Any of the above is a reasonable future phase on its own if a real
need for it shows up — this boundary reflects "not needed to call the
userland complete," not "impossible."

### Phase 6 — networking visibility (complete)
Opened to pick up the first item from Phase 5's closing boundary list:
"networking beyond `curl`/`ping`". Rather than `ip`/`ifconfig` (which
would need real netlink `rtnetlink` handling — a bigger, separate
scope decision, deferred for now, same spirit as `dig` needing async),
started with the more self-contained half of that gap:

- [x] `ss` (iproute2 — the modern tool a real Arch install actually
      ships, unlike the older `net-tools` `netstat`) — no new engine
      to vendor at all: the already-vendored `procfs` crate (used
      elsewhere for `ps`/`free`/`uptime`) directly exposes
      `net::{tcp,tcp6,udp,udp6,unix}()`, parsed from `/proc/net/*`;
      this file is only the listing/formatting layer on top, plus
      socket-inode-to-process mapping via `/proc/*/fd` — the exact
      technique `procfs`'s own module docs demonstrate for a
      netstat-alike, not something invented here.
      `-t`/`-u`/`-x` select socket tables (default `-t -u`); `-a`
      includes listening sockets, `-l` shows only listening ones,
      matching real `ss` semantics; `-p` maps sockets to owning
      processes. `-n` accepted as a no-op (never does name lookups in
      the first place).
      Verified: socket data (addresses, ports, states, process
      ownership) matches real `ss -tan`/`-uan`/`-p` exactly for every
      real socket on this dev machine (TCP/UDP, IPv4/IPv6, listening
      and established) — checked by direct comparison, not just
      spot-checking. Two documented, deliberate gaps:
      1. Column widths are fixed, not real `ss`'s dynamic
         content/terminal-width sizing — same standard as `ps aux`.
      2. `Send-Q` on `LISTEN` rows always reads `0` instead of real
         `ss`'s accept-queue backlog limit (e.g. `4096`) — confirmed,
         by reading `/proc/net/tcp`'s raw text directly, that this
         number simply isn't present there at all; real `ss` gets it
         from a netlink `sock_diag` query instead. A structural gap
         from the `/proc`-only approach, not a formatting choice —
         fixing it for real would mean vendoring a netlink client, a
         bigger decision left for later.
      Not implemented: raw/packet sockets, `-e`/`-i`/`-o` (extended
      TCP/timer info), filter expressions.

- [x] `ip` (iproute2) — real interface/address listing via genuine
      `rtnetlink` (`NETLINK_ROUTE`) requests, not `/proc` scraping:
      no `/proc` file exposes interface flags, hardware addresses, or
      address scopes the way netlink does. Resolved the open question
      from `ss`'s own entry above (a netlink dependency, like `dig`'s
      async one, needing a real decision) by finding a fully
      synchronous path: `netlink-sys` (plain blocking `AF_NETLINK`
      socket, none of its optional `tokio`/`mio`/`async-io` features
      enabled), `netlink-packet-core` (the generic netlink envelope),
      and `netlink-packet-route` (the actual `RTM_GETLINK`/
      `RTM_GETADDR` message/attribute types) — the same crates
      `rtnetlink` itself is built on, used directly instead of through
      its async wrapper. The request/multipart-dump/parse loop itself
      (`netlink_dump` in `ip_cmd.rs`) is hand-rolled, since `rtnetlink`
      only exposes that logic through its async API.
      `ip addr`/`ip link` (also `ip a`/`ip l`, and abbreviation-
      matched the way real `ip` itself matches object names by
      prefix) list every interface with its flags, MTU, and hardware
      address, and every address with its prefix length and scope.
      Verified against real `ip addr`/`ip link` on this dev machine:
      identical interfaces (by index and name), flags (bit-for-bit via
      `LinkFlags`, the same `IFF_*` kernel constants real `ip`
      decodes), MTU, and every hardware/IPv4/IPv6 address with its
      correct prefix length — checked address-by-address, not
      spot-checked. Address scope names are remapped to real `ip`'s
      own display table (`RT_SCOPE_UNIVERSE` → `global`, matching
      exactly) rather than left as the kernel's raw scope name.
      Verified a second time somewhere the first check couldn't reach:
      the real boot test's genuinely separate environment (its own net
      namespace via a real boot, not just a different process on the
      same host) — confirmed `ip addr` there correctly shows only
      `lo`, with `<LOOPBACK>` but *not* `UP` (accurate: this minimal
      boot has no `systemd-networkd`/`NetworkManager` bringing
      interfaces up, so genuinely nothing is configured yet) and zero
      addresses — a real, correctly-reported "nothing configured"
      state, not a bug.
      Not implemented (at the time `ip addr`/`ip link` were added):
      any write operation, `ip route`, `ip neigh`/`ip rule`, JSON
      output, filtering by device name.
- [x] `ip route` — extends the same `ip_cmd.rs`/`netlink_dump`
      machinery above with an `RTM_GETROUTE` dump (`RouteMessage`,
      `RouteAttribute` from the same already-vendored
      `netlink-packet-route`), no new crate needed. Lists the IPv4
      main routing table — matching real `ip route`'s own default
      family (IPv4-only unless `-6` is given) and its own default
      table filtering (the kernel dump returns every table — main,
      `local`, any custom one — unless filtered; real `ip route` with
      no arguments only shows `main`, so this filters
      `route.header.table == RT_TABLE_MAIN` to match, rather than
      passing the raw dump straight through, which was a real,
      caught-by-diffing-against-the-real-tool discrepancy: the first
      version showed several extra `local`-table entries — `lo`'s and
      each interface's own host/broadcast routes — real `ip route`
      doesn't show by default).
      Verified byte-for-byte (aside from real `ip`'s own trailing
      space) against real `ip route` on this dev machine: same
      default route (gateway, device, protocol, source, metric) and
      same on-link subnet route (device, `scope link`, source,
      metric).
      Not implemented: `ip -6 route`, any write operation, other
      route tables, `ip neigh`/`ip rule`, JSON output.

This closes out the concrete work opened for Phase 6: all three of
"networking beyond `curl`/`ping`" — socket state via `ss`, interface/
address state via `ip addr`/`ip link`, and routing-table state via
`ip route` — are done, verified, and — for the two pieces needing a
genuinely different environment to prove (`dmesg`, `mount`, and `ip
addr` in a real, unconfigured network namespace) — exercised by the
real boot test rather than just the dev host. `ip neigh`/`ip rule`/any
write operation remain open if a real need for them shows up, on the
same "not needed yet, not impossible" footing as Phase 5's own closing
boundary.

### Phase 7 — storage/device visibility (complete)
Picks up `lsblk` from Phase 5's closing list — it was originally
grouped there with `lspci`/`lsusb` under "needs a vendor/device ID
database," but that's only true for those two (`pci.ids`/`usb.ids`);
`lsblk`'s own default output needs neither, just `/sys/block`.

- [x] `lsblk` — no crate to vendor: reads `/sys/block/<dev>` and each
      device's own `size`/`ro`/`removable`/`dev` attribute files
      directly, the same reasoning as `dmesg`/`ss` needing no engine
      of their own. Default columns only (`NAME MAJ:MIN RM SIZE RO
      TYPE MOUNTPOINTS`), with the same tree-drawing (`├─`/`└─`) real
      `lsblk` uses for partitions under their parent disk.
      `MOUNTPOINTS` needed two lookup keys, not one, discovered by
      testing on two genuinely different real systems (this dev
      machine's btrfs root, and the boot test's ext4 root) rather than
      just one:
      - The real device path, read from `/proc/self/mountinfo`'s
        trailing mount-source field, resolves most filesystems
        correctly — except the *root* filesystem specifically, which
        the kernel always reports there as the synthetic `/dev/root`
        alias, never the real `root=` path used at boot (confirmed via
        the real boot test: `root=/dev/vda` still shows source
        `/dev/root`).
      - Falling back to `MAJ:MIN` (mountinfo's 3rd field) resolves
        that case correctly (ext4/vfat/etc. report the real device's
        actual number there) — except btrfs, which uses its own
        internal anon-block-device numbering for multi-subvolume
        mounts, confirmed directly on this dev machine: this box's
        btrfs `/`, `/home`, `/var/cache`, `/var/log` subvolumes all
        share one `MAJ:MIN` matching nothing in `/sys/block`, while
        each one's mount-source field correctly says the real
        `/dev/nvme0n1p2`.
      So: try the real device-path match first, fall back to
      `MAJ:MIN` only for the `/dev/root` case — the same two-technique
      approach real `lsblk` itself uses via `libmount`. Verified,
      after this fix, correct on both real systems: this dev machine's
      full btrfs multi-mountpoint tree, and the boot test's `vda`
      (major `254`, real virtio-blk) showing `/` correctly via the
      `MAJ:MIN` fallback.
      `SIZE` is a hand-rolled binary-prefix (K/M/G/T) formatter, close
      to but not guaranteed identical to real `lsblk`'s own rounding —
      same category of cosmetic gap as `free -h` elsewhere in this
      project.
      Not implemented: `-f` (filesystem type/UUID/label — needs
      `libblkid`), `-o` (custom columns), `-J`/`-P` (JSON/pairs
      output), device-mapper/LVM/RAID relationship trees.

- [x] `lspci`/`lsusb` — the vendor/device ID database each genuinely
      needs turned out to have a clean vendoring source after all:
      the `pci-ids`/`usb-ids` crates embed the real PCI/USB ID
      Repository databases (`pci.ids`/`usb.ids`) at compile time, the
      same databases real `lspci`/`lsusb` read from disk (usually
      `/usr/share/hwdata/` on Arch) — just compiled in instead of
      depending on that file being installed. Device enumeration
      itself needs no crate: a direct `/sys/bus/pci/devices/*` /
      `/sys/bus/usb/devices/*` read, same as `lsblk`.
      `lspci` (default output) came out byte-for-byte identical to
      real `lspci` on this dev machine (every bus address, class,
      vendor, device name, and revision, across ~25 real devices)
      after one real fix caught by diffing: real `lspci` omits the
      `(rev NN)` suffix entirely when the revision is `0` (its
      "nothing meaningful to report" value) — the first version always
      printed `(rev 00)`, confirmed wrong on two real devices (an NVMe
      controller and an Intel signal-processing device) before being
      fixed to match.
      `lsusb` matched real `lsusb` for every device's bus/device
      number and vendor:product ID, and for most device names — two
      real devices on this machine (a Wacom touch sensor, an IMC
      Networks camera) resolved their vendor name but not their exact
      product name, confirmed by querying the vendored database
      directly to be a real database-currency gap (that specific
      product ID isn't in the crate's `usb.ids` snapshot yet, though
      the vendor ID is), not a lookup bug. Falls back to vendor-name-
      only rather than blank in that case.
      Both verified a second way too: run from the real boot test
      against the VM's own, genuinely different hardware (virtio-blk/
      virtio-net PCI devices, no USB bus at all) — both exit cleanly
      with real output rather than crashing on unfamiliar topology.
      Not implemented: `-v`/`-vv`/`-k` (verbose PCI details, kernel
      driver), `-n`/`-nn` (numeric IDs), `-t` (USB tree view), `-d`
      (filter by vendor:product).

This closes Phase 5's original closing-list item cleanly: the
`lspci`/`lsusb` "needs a database" distinction turned out to have a
real, clean answer (vendor the ID-repository crates) rather than being
a permanent gap.

## Non-goals
Rewriting every package in the Arch repos (tens of thousands of packages,
most already upstream projects in their own languages) is not a software
task — it's not being attempted. "Fully in Rust" here means: Rust userland
core + Linux kernel, same as how "Arch Linux" is glibc/bash/systemd +
Linux kernel today.
