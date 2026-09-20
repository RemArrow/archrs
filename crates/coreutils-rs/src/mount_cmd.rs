//! `mount`/`umount` (util-linux) — real `mount(2)`/`umount2(2)` syscalls
//! via the `nix` crate's `mount` module, the same crate (and the exact
//! same `MsFlags` bitflags) `archrs-init` already uses for its own
//! boot-time `/proc`/`/sys`/`/dev` mounts, rather than hand-rolling the
//! raw `libc::mount` FFI call a second time in this crate.
//!
//! Scope: `mount` with no arguments lists currently mounted
//! filesystems by reading `/proc/self/mounts` and reformatting each
//! line as real `mount`'s default `source on target type fstype
//! (options)` output; `mount SOURCE TARGET [-t FSTYPE] [-o OPTIONS]`
//! performs a real mount, translating the common comma-separated
//! `-o` flags (`ro`/`rw`, `noexec`/`exec`, `nosuid`/`suid`,
//! `nodev`/`dev`, `noatime`/`atime`/`relatime`, `sync`/`async`,
//! `remount`, `bind`/`rbind`, `private`/`shared`/`slave`/
//! `unbindable`) into `MsFlags`, and passing anything else through
//! verbatim as filesystem-specific mount data (e.g. tmpfs's
//! `size=100m`) — the same split real `mount` itself makes. `umount
//! TARGET` (plus `-f`/`--force`, `-l`/`--lazy`) wraps `umount2`.
//!
//! Not implemented: `/etc/fstab` (`mount -a`, mounting by label/UUID
//! from a fstab entry), loop-device setup for mounting image files
//! (real `mount` auto-`losetup`s these; would need vendoring or
//! reimplementing that too), `--bind`'s single-argument short form
//! (`mount --bind src dst` still needs both explicit args here).
//!
//! Real mounts (anything but a handful of pseudo-filesystems already
//! mounted) need `CAP_SYS_ADMIN` — confirmed real `mount` fails
//! identically unprivileged on this dev machine. Verified for real as
//! root, the only way to verify any of this meaningfully: the boot
//! test now mounts a real `tmpfs` with `-o` options, confirms it shows
//! up correctly in `mount`'s own listing, writes through it, and
//! unmounts it, all as the init-spawned session's genuine root.

use nix::mount::{MntFlags, MsFlags, mount, umount2};
use std::ffi::OsString;
use std::fs;
use std::vec::IntoIter;

fn parse_options(opts: &str) -> (MsFlags, Vec<&str>) {
    let mut flags = MsFlags::empty();
    let mut data = Vec::new();
    for opt in opts.split(',').filter(|s| !s.is_empty()) {
        match opt {
            "ro" => flags.insert(MsFlags::MS_RDONLY),
            "rw" => flags.remove(MsFlags::MS_RDONLY),
            "noexec" => flags.insert(MsFlags::MS_NOEXEC),
            "exec" => flags.remove(MsFlags::MS_NOEXEC),
            "nosuid" => flags.insert(MsFlags::MS_NOSUID),
            "suid" => flags.remove(MsFlags::MS_NOSUID),
            "nodev" => flags.insert(MsFlags::MS_NODEV),
            "dev" => flags.remove(MsFlags::MS_NODEV),
            "noatime" => flags.insert(MsFlags::MS_NOATIME),
            "atime" => flags.remove(MsFlags::MS_NOATIME),
            "relatime" => flags.insert(MsFlags::MS_RELATIME),
            "norelatime" => flags.remove(MsFlags::MS_RELATIME),
            // Found the hard way: real systemd's generated `tmp.mount`
            // (`/etc/fstab`-independent, systemd's own built-in default
            // for `/tmp`) passes `strictatime` explicitly — with nothing
            // recognizing it, it fell all the way through to "opaque
            // filesystem data" and got handed to `tmpfs`'s own option
            // parser as a bare flag it doesn't understand, which
            // rejected it outright (`EINVAL`, kernel log: "tmpfs: Unknown
            // parameter 'strictatime'"), which surfaced here as
            // `mounting on /tmp failed: EINVAL`.
            "strictatime" => flags.insert(MsFlags::MS_STRICTATIME),
            "nostrictatime" => flags.remove(MsFlags::MS_STRICTATIME),
            "sync" => flags.insert(MsFlags::MS_SYNCHRONOUS),
            "async" => flags.remove(MsFlags::MS_SYNCHRONOUS),
            "remount" => flags.insert(MsFlags::MS_REMOUNT),
            "bind" => flags.insert(MsFlags::MS_BIND),
            "rbind" => flags.insert(MsFlags::MS_BIND | MsFlags::MS_REC),
            "private" => flags.insert(MsFlags::MS_PRIVATE),
            "shared" => flags.insert(MsFlags::MS_SHARED),
            "slave" => flags.insert(MsFlags::MS_SLAVE),
            "unbindable" => flags.insert(MsFlags::MS_UNBINDABLE),
            // Filesystem-specific data (tmpfs's `size=`, etc.) — passed
            // through verbatim, the same split real `mount` makes.
            other => data.push(other),
        }
    }
    (flags, data)
}

fn list_mounts() -> i32 {
    let contents = match fs::read_to_string("/proc/self/mounts") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mount: cannot read /proc/self/mounts: {e}");
            return 1;
        }
    };
    for line in contents.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        println!(
            "{} on {} type {} ({})",
            fields[0], fields[1], fields[2], fields[3]
        );
    }
    0
}

pub fn run_mount(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let mut positional = Vec::new();
    let mut fstype: Option<String> = None;
    let mut options: Option<String> = None;

    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "-t" | "--types" => {
                i += 1;
                fstype = argv.get(i).cloned();
            }
            "-o" | "--options" => {
                i += 1;
                options = argv.get(i).cloned();
            }
            other => positional.push(other.to_string()),
        }
        i += 1;
    }

    if positional.is_empty() {
        return list_mounts();
    }

    let (flags, data) = match &options {
        Some(o) => parse_options(o),
        None => (MsFlags::empty(), Vec::new()),
    };

    // Real `mount(8)`'s single-argument form — `mount -o remount,rw /` —
    // takes just the already-mounted target, not a source; the kernel's
    // own `mount(2)` for `MS_REMOUNT` identifies the superblock purely by
    // target and ignores the source string's contents (real `mount`
    // itself just resends whatever `/proc/mounts` lists there). Found
    // the hard way: real `systemd-remount-fs.service` execs exactly this
    // one-argument form for `/`, and this only ever accepted the real
    // two-argument `SOURCE TARGET` form, rejecting it as a usage error.
    let (source, target): (String, &String) = if positional.len() == 1 {
        if !flags.contains(MsFlags::MS_REMOUNT) {
            eprintln!("mount: usage: mount SOURCE TARGET [-t FSTYPE] [-o OPTIONS]");
            return 2;
        }
        ("none".to_string(), &positional[0])
    } else if positional.len() == 2 {
        (positional[0].clone(), &positional[1])
    } else {
        eprintln!("mount: usage: mount SOURCE TARGET [-t FSTYPE] [-o OPTIONS]");
        return 2;
    };
    let source = &source;
    let data_str = if data.is_empty() {
        None
    } else {
        Some(data.join(","))
    };

    match mount(
        Some(source.as_str()),
        target.as_str(),
        fstype.as_deref(),
        flags,
        data_str.as_deref(),
    ) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("mount: {source}: mounting on {target} failed: {e}");
            1
        }
    }
}

pub fn run_umount(args: IntoIter<OsString>) -> i32 {
    let argv: Vec<String> = args.map(|s| s.to_string_lossy().into_owned()).collect();
    let mut flags = MntFlags::empty();
    let mut target = None;
    for arg in argv.into_iter().skip(1) {
        match arg.as_str() {
            "-f" | "--force" => flags.insert(MntFlags::MNT_FORCE),
            "-l" | "--lazy" => flags.insert(MntFlags::MNT_DETACH),
            // Real systemd execs its own generated `.mount` units'
            // `ExecUnmount` as `umount -c TARGET` — found the hard way:
            // every real shutdown failed to unmount `/boot`/`/tmp`
            // ("umount: -c: ENOENT"), because `-c` wasn't recognized
            // here and got treated as the target path itself instead of
            // a flag. `-c`/`--no-canonicalize` tells real `umount` not
            // to canonicalize the target before looking it up (real
            // `umount(8)` does this so a mountpoint that's already
            // partway torn down, or a symlink component that no longer
            // resolves, doesn't block unmounting it) — this
            // implementation already passes the target straight to
            // `umount2` uncanonicalized, so it's accepted and ignored
            // rather than implemented.
            "-c" | "--no-canonicalize" => {}
            other => target = Some(other.to_string()),
        }
    }
    let Some(target) = target else {
        eprintln!("umount: usage: umount TARGET [-f] [-l]");
        return 2;
    };
    match umount2(target.as_str(), flags) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("umount: {target}: {e}");
            1
        }
    }
}
