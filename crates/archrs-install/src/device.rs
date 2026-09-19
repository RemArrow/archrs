//! Real block-device operations: partitioning, formatting, mounting.
//!
//! **Real root is required for all of this**, confirmed the hard way:
//! the original design tried to stay root-free by building plain
//! filesystem *images* and pointing `grub-install`/`mkinitcpio` at
//! plain directories (this project's usual technique — see
//! `scripts/lib-build-rootfs.sh`). `mkinitcpio` genuinely doesn't need
//! root or a real device for that. `grub-install --efi-directory`
//! does, for two separate real reasons hit one after another while
//! testing this: (1) even in "no-chroot" mode it opens the *real*
//! underlying block device of wherever `--efi-directory` lives to
//! probe it (confirmed: failed with a raw permission error reading
//! `/dev/nvme0n1p2`, not tmpfs-specific — it failed differently, but
//! still failed, even off tmpfs), and (2) once given real root, it
//! then correctly refused a plain directory outright ("doesn't look
//! like an EFI partition") — `--efi-directory` has to genuinely *be* a
//! mounted FAT/EFI filesystem, not just any directory. Real root and a
//! real mount are both unavoidable, so this module does the
//! conventional thing directly (partition, format, mount) rather than
//! working around it.
//!
//! Partitioning itself shells out to real `parted` rather than using
//! the `gpt` crate that was tried first — confirmed for real that
//! `parted`+`partprobe` reliably creates real `/dev/loop0p1`-style
//! partition device nodes on this dev machine (verified directly:
//! `parted -s ... mkpart ...` then `partprobe` produced them
//! immediately), while the `gpt` crate's own `GptDisk::write()`
//! produced a table the kernel's own partition scanner apparently
//! never recognized (`/dev/loop0p1` never appeared, even after the
//! exact same `partprobe` call that worked seconds later against a
//! `parted`-written table on the same loop device). Root-caused no
//! further than that — given every other privileged step here already
//! shells out to a real system tool (`mkfs.fat`, `mke2fs`,
//! `grub-install`, `mkinitcpio`), doing the same for partitioning is
//! actually more consistent with this tool's own pattern, not less.
//! Partition GUIDs are read back afterward via real `blkid`, rather
//! than pre-generated and handed to a table-writer, since `parted`
//! itself assigns them.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

const ESP_SIZE_MIB: u64 = 1024; // 1 GiB
const ESP_START_MIB: u64 = 1; // real convention: 1 MiB alignment gap before the first partition

pub struct PartitionLayout {
    pub esp_guid: uuid::Uuid,
    pub esp_dev: PathBuf,
    pub root_guid: uuid::Uuid,
    pub root_dev: PathBuf,
}

fn run(cmd: &mut Command) -> Result<()> {
    let desc = format!("{cmd:?}");
    let status = cmd.status().with_context(|| format!("spawning {desc}"))?;
    if !status.success() {
        bail!("{desc} failed: {status}");
    }
    Ok(())
}

/// `/dev/sda` -> `/dev/sda1`; `/dev/nvme0n1`/`/dev/loop0` (names ending
/// in a digit) -> `/dev/nvme0n1p1`/`/dev/loop0p1` — the two real Linux
/// partition-device-node naming conventions.
fn partition_device_path(base: &Path, index: u32) -> PathBuf {
    let base_str = base.to_string_lossy();
    if base_str.ends_with(|c: char| c.is_ascii_digit()) {
        PathBuf::from(format!("{base_str}p{index}"))
    } else {
        PathBuf::from(format!("{base_str}{index}"))
    }
}

/// Attaches `file` as a loop device (`losetup -f --show`) — used for
/// `--output-image FILE`, so the exact same real partition/format/mount
/// code path runs regardless of whether the final target is a plain
/// file or a real disk.
pub fn attach_loop(file: &Path) -> Result<PathBuf> {
    // `-P`/`--partscan`: without it, the loop driver never creates
    // partition sub-devices at all (confirmed for real: without this,
    // `/dev/loop0p1` never appeared even after `partprobe` on the
    // already-attached device — partition-scan support has to be
    // requested at attach time, `partprobe` alone can't retrofit it).
    let out = Command::new("losetup")
        .arg("-f")
        .arg("-P")
        .arg("--show")
        .arg(file)
        .output()
        .context("running losetup")?;
    if !out.status.success() {
        bail!("losetup failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let dev = String::from_utf8(out.stdout)
        .context("losetup output wasn't UTF-8")?
        .trim()
        .to_string();
    Ok(PathBuf::from(dev))
}

pub fn detach_loop(dev: &Path) -> Result<()> {
    run(Command::new("losetup").arg("-d").arg(dev))
}

/// Waits for the kernel to actually create `path` after a partition
/// rescan — `partprobe` returning doesn't guarantee the device node has
/// appeared yet.
fn wait_for(path: &Path) -> Result<()> {
    for _ in 0..50 {
        if path.exists() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!("{path:?} never appeared after partitioning")
}

fn read_partuuid(dev: &Path) -> Result<uuid::Uuid> {
    let out = Command::new("blkid")
        .arg("-s")
        .arg("PARTUUID")
        .arg("-o")
        .arg("value")
        .arg(dev)
        .output()
        .context("running blkid")?;
    if !out.status.success() {
        bail!(
            "blkid {dev:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let s = String::from_utf8(out.stdout)
        .context("blkid output wasn't UTF-8")?
        .trim()
        .to_string();
    uuid::Uuid::parse_str(&s).with_context(|| format!("parsing PARTUUID {s:?} from {dev:?}"))
}

/// Creates a real GPT partition table on `dev` (a real disk or a loop
/// device) via real `parted`, then makes the kernel notice the new
/// partitions (`partprobe`), waits for their device nodes to actually
/// show up, and reads back the real PARTUUID `parted` assigned each
/// one (via `blkid`) for `fstab.rs`/`build::write_grub_cfg` to
/// reference.
pub fn partition(dev: &Path, total_size: u64) -> Result<PartitionLayout> {
    let total_mib = total_size / (1024 * 1024);
    let esp_end_mib = ESP_START_MIB + ESP_SIZE_MIB;
    if esp_end_mib >= total_mib {
        bail!("disk too small for a 1 GiB ESP plus a real root partition");
    }

    run(Command::new("parted")
        .arg("-s")
        .arg(dev)
        .arg("mklabel")
        .arg("gpt")
        .arg("mkpart")
        .arg("archrs-esp")
        .arg("fat32")
        .arg(format!("{ESP_START_MIB}MiB"))
        .arg(format!("{esp_end_mib}MiB"))
        .arg("set")
        .arg("1")
        .arg("esp")
        .arg("on")
        .arg("mkpart")
        .arg("archrs-root")
        .arg("ext4")
        .arg(format!("{esp_end_mib}MiB"))
        .arg("100%"))?;

    run(Command::new("partprobe").arg(dev))?;

    let esp_dev = partition_device_path(dev, 1);
    let root_dev = partition_device_path(dev, 2);
    wait_for(&esp_dev)?;
    wait_for(&root_dev)?;

    Ok(PartitionLayout {
        esp_guid: read_partuuid(&esp_dev)?,
        esp_dev,
        root_guid: read_partuuid(&root_dev)?,
        root_dev,
    })
}

pub fn format_partitions(layout: &PartitionLayout) -> Result<()> {
    run(Command::new("mkfs.fat").arg("-F32").arg(&layout.esp_dev))?;
    run(Command::new("mke2fs")
        .arg("-F")
        .arg("-t")
        .arg("ext4")
        .arg("-L")
        .arg("archrsroot")
        .arg(&layout.root_dev))
}

pub fn mount_all(layout: &PartitionLayout, mountpoint: &Path) -> Result<()> {
    std::fs::create_dir_all(mountpoint)?;
    run(Command::new("mount").arg(&layout.root_dev).arg(mountpoint))?;
    let boot = mountpoint.join("boot");
    std::fs::create_dir_all(&boot)?;
    run(Command::new("mount").arg(&layout.esp_dev).arg(&boot))
}

pub fn unmount_all(mountpoint: &Path) -> Result<()> {
    run(Command::new("umount").arg(mountpoint.join("boot")))?;
    run(Command::new("umount").arg(mountpoint))
}
