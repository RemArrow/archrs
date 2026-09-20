//! `archrs-install` — a real installer, producing a genuinely bootable
//! GPT/UEFI disk image (GRUB + a real Linux kernel + a real `mkinitcpio`
//! initramfs + this project's own userland), for actual hardware or
//! disks, not just the throwaway QEMU test images `scripts/*.sh` build
//! and rebuild on every run. Phase 12 — the thing that turns "a bunch
//! of verified components" into a system someone could actually have.
//!
//! # Needs real root — unlike everything else in this project
//!
//! Every other tool here deliberately avoids needing host root (see
//! `scripts/boot-test.sh`'s own top-of-file comment) by building
//! filesystem *images* rather than mounting anything. The first version
//! of this tool tried the same trick — build the ESP/root filesystems
//! as plain images, point `mkinitcpio -r`/`grub-install --efi-directory`
//! at plain directories, splice the images together into one GPT file
//! with the `gpt` crate. `mkinitcpio` genuinely doesn't care. Real
//! `grub-install` does, for two separate reasons hit one after another
//! while actually testing this (not assumed): (1) even pointed at a
//! plain directory, it opens the real underlying block device of
//! wherever that directory lives, and fails without permission to read
//! it (confirmed: a real `EACCES`-shaped failure reading this dev
//! machine's own `/dev/nvme0n1p2`, resolved by re-running as real
//! root); and (2), once genuinely root, it then correctly *refused* a
//! plain directory outright — `error: ... doesn't look like an EFI
//! partition` — because `--efi-directory` has to actually *be* a
//! mounted FAT/EFI filesystem, not just any directory. Both are real
//! `grub-install` behavior, not something to work around with a flag;
//! this tool does the conventional thing instead: partition, format,
//! mount, install, matching how a real Arch install is normally done
//! by hand. Run this with `sudo`.
//!
//! # What's real here and what isn't
//! Kernel module coverage for the initramfs uses the `mkinitcpio`
//! package's own stock default `HOOKS` (`block`+`filesystems` already
//! cover generic storage/filesystem drivers without needing
//! `autodetect`, which only helps trim the image down, not correctness
//! — verified this holds by actually booting the result, not assumed).
//! `root=PARTUUID=...` in the kernel command line is a genuine kernel
//! built-in (`name_to_dev_t`), used here even though real `udev` is
//! now present (systemd absorbed it — Phase 13 replaced this
//! project's own init with real systemd, which means `/dev/disk/by-*`
//! symlinks genuinely do get populated at boot now, unlike every
//! earlier phase's own honest "no udev/mdev equivalent yet" caveat):
//! `PARTUUID=` is simpler and doesn't depend on udev having finished
//! settling before the root filesystem is looked for. The kernel
//! package name itself is resolved at runtime
//! (`build::kernel_package_name`) — vanilla Arch's plain `linux`
//! doesn't exist on Manjaro's own mirrors, confirmed for real.
//! Automatic hardware/module loading (hotplug via real `udevd`, not
//! just the fixed set of modules this project's own `insmod`/
//! `modprobe` can load by hand) is also real now, for the same reason
//! — a genuine capability gain from switching to systemd, not just a
//! neutral swap.
//!
//! `--output-image FILE` (the default, and what this project's own
//! testing uses) builds against a loop device backing that file — the
//! same real partition/format/mount code path as `--target DEVICE`,
//! just on a file instead of a real disk, so what gets verified is the
//! real thing, not a simplified stand-in. `--target /dev/sdX` is real
//! and implemented, gated behind real safety checks (refuses a mounted
//! device or the running system's own disk, requires typing the exact
//! device path back), but has deliberately not been run against a real
//! device by anyone building this project so far — verified instead by
//! building to a file and booting *that* for real in QEMU with OVMF
//! (real UEFI firmware, not `-kernel` direct boot like every other test
//! script here), which exercises the entire real GRUB → kernel →
//! initramfs → systemd chain.
//!
//! Not implemented: BIOS/MBR boot (UEFI only), multiple disks, LVM/
//! LUKS/RAID, resizing an existing installation, `/home` as a separate
//! partition, swap.

mod build;
mod device;
mod fstab;
mod safety;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;

/// Build a real, bootable archrs disk image (GPT + UEFI/GRUB + a real
/// kernel/initramfs + this project's own userland). Must be run as
/// root.
#[derive(Parser)]
#[command(name = "archrs-install")]
struct Cli {
    /// Write the finished image to this plain file instead of a real
    /// device — the safe, default way to use this tool. Flash it
    /// yourself, or boot it directly in a hypervisor.
    #[arg(long, conflicts_with = "target")]
    output_image: Option<PathBuf>,

    /// Install directly onto this block device — a real, destructive
    /// operation. Refuses a mounted device or the device backing the
    /// currently-running system, and requires typing the exact device
    /// path back to confirm.
    #[arg(long, conflicts_with = "output_image")]
    target: Option<PathBuf>,

    /// Image size for --output-image (ignored for --target, which
    /// uses the real device's own size).
    #[arg(long, default_value_t = 8)]
    size_gib: u64,

    /// Skip typing the device path back to confirm --target. For
    /// scripted/CI use only — never pass this against a real disk you
    /// haven't triple-checked yourself.
    #[arg(long)]
    yes_i_am_sure: bool,

    /// Set this as the root account's password. Real `shadow` ships
    /// root locked by default (confirmed for real: `root:*:...` in a
    /// built image's own `/etc/shadow`, and `sulogin` refusing even
    /// emergency-mode console access as a result) — omit this and
    /// archrs-install generates a random one and prints it once, so a
    /// fresh install is never silently unrecoverable.
    #[arg(long)]
    root_password: Option<String>,
}

/// Real entropy, not a PRNG seeded from time/pid — `/dev/urandom` is
/// already guaranteed present (it's the kernel's own device node, not
/// something a package installs), so this needs no new dependency.
/// Hex-encoded rather than raw bytes so the printed password is safe to
/// read off a terminal and retype by hand.
fn generate_password() -> Result<String> {
    use std::io::Read;
    let mut buf = [0u8; 12];
    std::fs::File::open("/dev/urandom")
        .context("opening /dev/urandom")?
        .read_exact(&mut buf)
        .context("reading /dev/urandom")?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn require_root() -> Result<()> {
    // SAFETY: geteuid() has no failure mode and takes no arguments.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        bail!("archrs-install must be run as root (try: sudo archrs-install ...)");
    }
    Ok(())
}

fn main() -> Result<()> {
    require_root()?;
    let cli = Cli::parse();

    if cli.output_image.is_none() && cli.target.is_none() {
        bail!("specify either --output-image FILE or --target DEVICE");
    }

    // For --target, run every safety check *before* touching anything —
    // partitioning is the first real, destructive step.
    if let Some(target) = &cli.target {
        safety::run_safety_checks(target)?;
        safety::confirm(target, cli.yes_i_am_sure)?;
    }

    let workdir = workdir()?;
    let mountpoint = workdir.join("mnt");

    let (device_path, loop_dev, size_bytes) = match (&cli.output_image, &cli.target) {
        (Some(out), None) => {
            let size = cli.size_gib * 1024 * 1024 * 1024;
            {
                let f = std::fs::File::create(out).with_context(|| format!("creating {out:?}"))?;
                f.set_len(size)?;
            }
            let loop_dev = device::attach_loop(out)?;
            println!("archrs-install: {out:?} attached as {loop_dev:?}");
            (loop_dev.clone(), Some(loop_dev), size)
        }
        (None, Some(target)) => {
            let size = safety::device_size_bytes(target)?;
            (target.clone(), None, size)
        }
        _ => unreachable!("checked above"),
    };

    let (root_password, password_was_generated) = match cli.root_password {
        Some(p) => (p, false),
        None => (generate_password()?, true),
    };

    let result = run_install(&device_path, size_bytes, &mountpoint, &root_password);

    if let Some(loop_dev) = &loop_dev {
        let _ = device::detach_loop(loop_dev);
    }

    if result.is_ok() && password_was_generated {
        println!(
            "archrs-install: generated root password (no --root-password given): {root_password}"
        );
        println!("archrs-install: log in as root with it and change it with 'passwd' — this is the only copy");
    }

    result
}

fn run_install(
    device_path: &std::path::Path,
    size_bytes: u64,
    mountpoint: &std::path::Path,
    root_password: &str,
) -> Result<()> {
    println!("archrs-install: partitioning {device_path:?}...");
    let layout = device::partition(device_path, size_bytes)?;

    println!("archrs-install: formatting partitions...");
    device::format_partitions(&layout)?;

    println!("archrs-install: mounting...");
    device::mount_all(&layout, mountpoint)?;

    let install_result = (|| -> Result<()> {
        println!("archrs-install: building base system into {mountpoint:?}...");
        build::build_rootfs(mountpoint)?;

        println!("archrs-install: running mkinitcpio...");
        let kernel_version = build::kernel_version(mountpoint)?;
        build::run_mkinitcpio(mountpoint, &kernel_version)?;

        println!("archrs-install: copying kernel image...");
        build::copy_kernel_image(mountpoint, &kernel_version)?;

        println!("archrs-install: running grub-install...");
        build::run_grub_install(mountpoint)?;

        println!("archrs-install: writing fstab/hostname/hosts...");
        fstab::write(mountpoint, layout.root_guid, layout.esp_guid)?;

        println!("archrs-install: writing grub.cfg...");
        build::write_grub_cfg(mountpoint, layout.root_guid)?;

        println!("archrs-install: enabling systemd services...");
        build::enable_services(mountpoint)?;

        println!("archrs-install: setting root password...");
        build::set_root_password(mountpoint, root_password)?;

        Ok(())
    })();

    println!("archrs-install: unmounting...");
    device::unmount_all(mountpoint)?;

    install_result?;
    println!("archrs-install: done — {device_path:?} is a real, bootable archrs install");
    Ok(())
}

fn workdir() -> Result<PathBuf> {
    let dir = std::env::current_dir()
        .context("determining current directory")?
        .join(format!("archrs-install-work-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
