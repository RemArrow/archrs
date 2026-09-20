//! Builds the base system onto an already-mounted target (real packages
//! via `pacman-rs`, this project's own binaries, `mkinitcpio`,
//! `grub-install`) — chroot-free (`mkinitcpio -r`/`grub-install
//! --efi-directory` both operate on a directory tree directly, no
//! `chroot(2)` needed), but not mount-free — see `device.rs`'s own doc
//! comment for why a real mount turned out to be unavoidable.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The utility list is embedded at compile time from `coreutils-rs`'s
/// own source of truth, the same way `scripts/lib-build-rootfs.sh`
/// greps it at build-script time — so this can never drift out of sync
/// with it, and the resulting `archrs-install` binary doesn't need the
/// source tree available at runtime.
const UTIL_LIST_SRC: &str = include_str!("../../coreutils-rs/src/util_list.rs");

fn util_names() -> Vec<String> {
    UTIL_LIST_SRC
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            if !line.starts_with('(') {
                return None;
            }
            let after_paren = line.strip_prefix('(')?.trim_start();
            let after_quote = after_paren.strip_prefix('"')?;
            let end = after_quote.find('"')?;
            Some(after_quote[..end].to_string())
        })
        .collect()
}

fn sibling_bin(name: &str) -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe().context("locating our own executable")?;
    let dir = exe
        .parent()
        .context("our own executable has no parent directory")?;
    let candidate = dir.join(name);
    if !candidate.is_file() {
        bail!(
            "{candidate:?} not found — build the whole workspace with 'cargo build --release' first"
        );
    }
    Ok(candidate)
}

fn run(cmd: &mut Command) -> Result<()> {
    let desc = format!("{cmd:?}");
    let status = cmd.status().with_context(|| format!("spawning {desc}"))?;
    if !status.success() {
        bail!("{desc} failed: {status}");
    }
    Ok(())
}

/// Real base packages, the same list `scripts/lib-build-rootfs.sh`
/// installs, plus the three this project's test images never needed:
/// `mkinitcpio` (real initramfs generation), `grub` (real UEFI
/// bootloader), and `systemd` (this project's real init as of
/// Phase 13 — see ROADMAP.md). `dbus` is `systemctl`'s own real
/// dependency for talking to PID 1. The kernel package itself is
/// resolved separately by `kernel_package_name` below — its real name
/// isn't the same across mirrors.
const BASE_PACKAGES: &[&str] = &[
    "glibc",
    "filesystem",
    "bash",
    "xz",
    "file",
    "pam",
    "sudo",
    "shadow",
    "util-linux",
    "kmod",
    "mkinitcpio",
    "grub",
    "systemd",
    "dbus",
    // `fsck.vfat` — found the hard way by actually booting a real
    // install: without it, `systemd-fsck@.service` for the ESP (fstab
    // checks it, fsck pass "2") has no checker to exec for a `vfat`
    // filesystem at all, and the resulting `boot.mount` job sat stalled
    // until systemd's own default job timeout, dropping the boot into
    // emergency mode. `fsck`/`systemd-fsck` themselves come from
    // `util-linux`/`systemd` above; the actual `fsck.vfat` binary is
    // `dosfstools`'s alone.
    "dosfstools",
];

/// Vanilla Arch ships a plain `linux` metapackage; Manjaro doesn't —
/// confirmed for real (a genuine `pacman-rs -S linux` failure on this
/// dev machine's own Manjaro mirrors: "unresolved dependencies:
/// linux") — its repos instead carry versioned packages named
/// `linux<major><minor>` with no separator (`linux71` for `7.1.x`,
/// `linux612` for `6.12.x`, confirmed directly against this machine's
/// own synced `core.db`). Detected from `uname -r` containing
/// "MANJARO" rather than hardcoding one version, so this keeps working
/// as Manjaro's own recommended kernel version moves — real vanilla
/// Arch (no "MANJARO" in `uname -r`) still just gets plain `linux`.
fn kernel_package_name() -> String {
    let release = std::str::from_utf8(
        &std::process::Command::new("uname")
            .arg("-r")
            .output()
            .map(|o| o.stdout)
            .unwrap_or_default(),
    )
    .unwrap_or("")
    .trim()
    .to_string();
    if !release.to_uppercase().contains("MANJARO") {
        return "linux".to_string();
    }
    let mut parts = release.split('.');
    let major = parts.next().unwrap_or("");
    let minor = parts.next().unwrap_or("");
    if major.is_empty() || minor.is_empty() || !major.chars().all(|c| c.is_ascii_digit()) {
        return "linux".to_string(); // can't parse — fall back and let pacman-rs report the real error
    }
    format!("linux{major}{minor}")
}

pub fn build_rootfs(rootfs_dir: &Path) -> Result<()> {
    let pacman_rs = sibling_bin("pacman-rs")?;
    let kernel_pkg = kernel_package_name();
    println!("archrs-install: installing kernel package '{kernel_pkg}'");

    run(Command::new(&pacman_rs)
        .arg("-Sy")
        .arg("--root")
        .arg(rootfs_dir))?;
    run(Command::new(&pacman_rs)
        .arg("-S")
        .args(BASE_PACKAGES)
        .arg(&kernel_pkg)
        .arg("--root")
        .arg(rootfs_dir))?;

    std::fs::create_dir_all(rootfs_dir.join("usr/local/bin"))?;
    for bin in [
        "coreutils-rs",
        "pacman-rs",
        "makepkg-rs",
        "crond",
        "crontab",
    ] {
        std::fs::copy(
            sibling_bin(bin)?,
            rootfs_dir.join("usr/local/bin").join(bin),
        )
        .with_context(|| format!("copying {bin}"))?;
    }

    // su/sudo/visudo: real standalone setuid-root binaries, deliberately
    // not part of coreutils-rs's multicall dispatch — see
    // crates/privtools-rs's own doc comment for why.
    std::fs::create_dir_all(rootfs_dir.join("usr/bin"))?;
    for bin in ["su", "sudo", "visudo"] {
        let dest = rootfs_dir.join("usr/bin").join(bin);
        std::fs::copy(sibling_bin(bin)?, &dest)?;
        let mut perms = std::fs::metadata(&dest)?.permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o4755);
        std::fs::set_permissions(&dest, perms)?;
    }

    for util in util_names() {
        let link = rootfs_dir.join("usr/bin").join(&util);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink("/usr/local/bin/coreutils-rs", &link)
            .with_context(|| format!("symlinking {util}"))?;
    }

    Ok(())
}

/// Real Arch convention: the installed kernel's module directory is
/// named after its own version string, under `/usr/lib/modules/`.
/// Exactly one should exist for a fresh `linux` install.
pub fn kernel_version(rootfs_dir: &Path) -> Result<String> {
    let modules_dir = rootfs_dir.join("usr/lib/modules");
    let mut versions: Vec<String> = std::fs::read_dir(&modules_dir)
        .with_context(|| format!("reading {modules_dir:?}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    versions.sort();
    versions
        .into_iter()
        .next()
        .context("no kernel module directory found under /usr/lib/modules")
}

/// `-r`/`-c`/`-k`/`-g`: real flags for building an initramfs for a
/// *different* root than `/`, without chrooting — see this crate's own
/// top-of-file doc comment.
pub fn run_mkinitcpio(rootfs_dir: &Path, kernel_version: &str) -> Result<()> {
    std::fs::create_dir_all(rootfs_dir.join("boot"))?;
    run(Command::new("mkinitcpio")
        .arg("-r")
        .arg(rootfs_dir)
        .arg("-c")
        .arg(rootfs_dir.join("etc/mkinitcpio.conf"))
        .arg("-k")
        .arg(kernel_version)
        .arg("-g")
        .arg(rootfs_dir.join("boot/initramfs-linux.img")))
}

/// The real, packaged kernel image lives at
/// `/usr/lib/modules/<kernelver>/vmlinuz` — confirmed for real (this
/// dev machine's own `/usr/lib/modules/$(uname -r)/vmlinuz` exists as a
/// real file). What actually populates `/boot/vmlinuz-*` on a normal
/// install is a **pacman post-install hook** copying it there under a
/// distro-specific name (Manjaro's own convention turned out to be
/// `vmlinuz-<major>.<minor>-x86_64`, confirmed by listing this dev
/// machine's real `/boot` — not `vmlinuz-linux` like vanilla Arch).
/// `pacman-rs` deliberately doesn't run install hooks for a `--root
/// DIR` install (same real reason `mkinitcpio` needed a manual run
/// above: a hook meant for the live system shouldn't act on a fake
/// target root) — confirmed by the first version of this failing with
/// "no vmlinuz-* file found" despite the kernel package installing
/// cleanly. Copied here under this tool's own fixed name
/// (`vmlinuz-linux`, matching `initramfs-linux.img`'s own naming
/// above) rather than trying to replicate whatever hook-driven name a
/// given mirror's kernel package happens to use.
pub fn copy_kernel_image(rootfs_dir: &Path, kernel_version: &str) -> Result<()> {
    let src = rootfs_dir
        .join("usr/lib/modules")
        .join(kernel_version)
        .join("vmlinuz");
    let dest = rootfs_dir.join("boot/vmlinuz-linux");
    std::fs::copy(&src, &dest)
        .with_context(|| format!("copying kernel image {src:?} to {dest:?}"))?;
    Ok(())
}

/// `--efi-directory`/`--boot-directory` point at `rootfs_dir/boot`,
/// which by this point in `main.rs`'s orchestration is a *real* mounted
/// ESP (see `device.rs`'s own doc comment on why that's required, found
/// the hard way). `--removable` also writes the fallback
/// `EFI/BOOT/BOOTX64.EFI` path real firmware falls back to when no
/// NVRAM boot entry exists (which it won't, since this never touches
/// the real machine's own UEFI variables).
pub fn run_grub_install(rootfs_dir: &Path) -> Result<()> {
    run(Command::new("grub-install")
        .arg("--target=x86_64-efi")
        .arg(format!(
            "--efi-directory={}",
            rootfs_dir.join("boot").display()
        ))
        .arg(format!(
            "--boot-directory={}",
            rootfs_dir.join("boot").display()
        ))
        .arg("--removable")
        .arg("--bootloader-id=archrs")
        .arg("--no-nvram"))
}

/// Hand-written rather than `grub-mkconfig` (which wants to run
/// `os-prober`/filesystem detection against the *live* system it's
/// invoked on, not a cross-built target) — a minimal, fully predictable
/// menu is all a fresh install needs. `root=PARTUUID=...` is a genuine
/// kernel built-in (`name_to_dev_t`), not a udev-dependent lookup.
/// `/vmlinuz-linux` and `/initramfs-linux.img` are this tool's own
/// fixed names (see `copy_kernel_image`/`run_mkinitcpio` above), not
/// whatever a given mirror's kernel package would have
/// hook-generated. No `init=` override — the kernel's own default,
/// `/sbin/init`, is a real symlink the `systemd` package itself ships
/// (`/sbin/init -> ../lib/systemd/systemd`, confirmed directly on this
/// dev machine), so it already resolves to real systemd once that
/// package and `filesystem` (which sets up the `/sbin -> usr/bin`
/// merged-usr symlink) are both installed.
pub fn write_grub_cfg(rootfs_dir: &Path, root_guid: uuid::Uuid) -> Result<()> {
    let cfg = format!(
        "set timeout=3\n\
         set default=0\n\
         \n\
         menuentry \"archrs\" {{\n\
         \tlinux /vmlinuz-linux root=PARTUUID={root_guid} rw console=ttyS0\n\
         \tinitrd /initramfs-linux.img\n\
         }}\n"
    );
    std::fs::create_dir_all(rootfs_dir.join("boot/grub"))?;
    std::fs::write(rootfs_dir.join("boot/grub/grub.cfg"), cfg)?;
    Ok(())
}

/// Real `shadow` ships root's own `/etc/shadow` entry locked (`root:*:...`
/// on this dev machine's own build, confirmed directly by reading it back
/// out of a built image with `debugfs`) — there's no install hook to
/// unlock it, the same class of gap as `systemd-firstboot` above. Left
/// alone, that's not just "no root login": `sulogin` (real emergency-mode
/// console, real single-user mode) refuses outright with "Cannot open
/// access to console, the root account is locked" — confirmed for real by
/// actually landing a build in emergency mode (the `dosfstools` gap above)
/// and finding there was no way in at all, not even to diagnose it. Uses
/// real `chpasswd --root DIR`, the same chroot-free-offline-root pattern
/// `systemctl --root=` uses elsewhere in this file — not a hand-rolled
/// shadow-file edit.
pub fn set_root_password(rootfs_dir: &Path, password: &str) -> Result<()> {
    use std::io::Write;
    let mut child = Command::new("chpasswd")
        .arg("--root")
        .arg(rootfs_dir)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("spawning chpasswd")?;
    child
        .stdin
        .take()
        .context("chpasswd has no stdin")?
        .write_all(format!("root:{password}\n").as_bytes())
        .context("writing to chpasswd's stdin")?;
    let status = child.wait().context("waiting for chpasswd")?;
    if !status.success() {
        bail!("chpasswd failed: {status}");
    }
    Ok(())
}

/// A fresh, un-hooked `systemd` install has nothing enabled — no login
/// prompt, no networking. `systemctl --root=DIR enable UNIT` is real,
/// documented offline support (systemd's own manual: "Edit/enable/
/// disable/mask unit files in the specified root directory") built for
/// exactly this — an installer running without a chroot or a live
/// systemd instance to talk to, the same shape as `grub-install`'s own
/// `--boot-directory` and `mkinitcpio`'s `--moduleroot`.
///
/// Enables real `getty@tty1.service` (local console) and
/// `serial-getty@ttyS0.service` (harmless on real hardware with no
/// serial port — it just never receives input) — the same real
/// `agetty`/`login`/PAM chain already verified end to end in
/// `scripts/login-test.sh`, now started by systemd instead of this
/// project's own former `archrs-init`. `systemd-networkd.service` plus
/// a wildcard DHCP `.network` file (`Name=en* eth*`, not a specific
/// interface name — real predictable network interface naming, which
/// having real `systemd`/`udev` now actually enables, means this can't
/// assume `eth0` the way this project's own `dhcp_cmd.rs` test does)
/// gets the machine onto a network automatically at boot without
/// needing to know its real interface name ahead of time. This
/// project's own `crond`/`dhcpc` (`crond.service`, a small unit file
/// written here since no package ships one for this project's own
/// binary) stay real and available either way — `systemd-networkd`
/// handles automatic boot-time DHCP, `dhcpc` remains real and usable
/// by hand.
pub fn enable_services(rootfs_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(rootfs_dir.join("etc/systemd/network"))?;
    std::fs::write(
        rootfs_dir.join("etc/systemd/network/20-dhcp.network"),
        "[Match]\nName=en* eth*\n\n[Network]\nDHCP=yes\n",
    )?;

    let unit_dir = rootfs_dir.join("etc/systemd/system");
    std::fs::create_dir_all(&unit_dir)?;
    std::fs::write(
        unit_dir.join("archrs-cron.service"),
        "[Unit]\n\
         Description=archrs single-user cron daemon\n\
         \n\
         [Service]\n\
         ExecStart=/usr/local/bin/crond\n\
         Restart=on-failure\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n",
    )?;

    for unit in [
        "getty@tty1.service",
        "serial-getty@ttyS0.service",
        "systemd-networkd.service",
        "archrs-cron.service",
    ] {
        run(Command::new("systemctl")
            .arg(format!("--root={}", rootfs_dir.display()))
            .arg("enable")
            .arg(unit))?;
    }

    // `systemd-firstboot.service` is a real, "static" unit (confirmed via
    // `systemctl list-unit-files` — "static" units have no [Install]
    // section, so `mask` rather than `disable` is the correct way to
    // turn one off) that runs unconditionally on the very first real
    // boot and, on a genuinely unconfigured system, interactively
    // prompts on the console for a timezone/locale — confirmed the hard
    // way while testing this project's own boot-test.sh: it hung at
    // "Please enter the new timezone name..." until an outer timeout
    // killed it. Pre-populating what it would have asked for, then
    // masking it, avoids a real install ever hitting that same prompt
    // on someone's actual first boot.
    std::os::unix::fs::symlink("/usr/share/zoneinfo/UTC", rootfs_dir.join("etc/localtime"))
        .or_else(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(e)
            }
        })
        .context("symlinking /etc/localtime")?;
    std::fs::write(rootfs_dir.join("etc/locale.conf"), "LANG=C.UTF-8\n")?;
    run(Command::new("systemctl")
        .arg(format!("--root={}", rootfs_dir.display()))
        .arg("mask")
        .arg("systemd-firstboot.service"))?;

    Ok(())
}
