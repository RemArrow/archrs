//! `lsblk` (util-linux) — lists block devices. No crate to vendor: the
//! actual data comes straight from `/sys/block/<dev>` (and each
//! device's own `size`/`ro`/`removable`/`dev` attribute files), which
//! is simple enough to read directly — same reasoning as `dmesg`/`ss`
//! elsewhere in this crate needing no engine of their own. Real
//! `lsblk` additionally consults `libblkid` for filesystem type/UUID/
//! label (its `-f` output) and udev for richer device metadata; this
//! implementation covers the default column set only, which needs
//! neither.
//!
//! Scope: default columns (`NAME MAJ:MIN RM SIZE RO TYPE MOUNTPOINTS`)
//! for whole disks and their partitions, with the same tree-drawing
//! (`├─`/`└─`) real `lsblk` uses. `TYPE` is inferred from the device
//! name (`loop*` → `loop`, `sr*` → `rom`, else `disk` for a top-level
//! `/sys/block` entry or `part` for one of its subdirectories) rather
//! than a real ioctl-based capability query, since name-based
//! inference already covers every real device on this dev machine and
//! everything `pacman-rs`/`archrs-init` deal with. `MOUNTPOINTS`
//! cross-references `/proc/self/mountinfo` by `MAJ:MIN` device number
//! (not by device path string — the kernel reports the real root
//! filesystem there as the synthetic `/dev/root` alias regardless of
//! the actual `root=` path used at boot, confirmed via the real boot
//! test; matching by device number instead, the same technique real
//! `lsblk` itself uses, sidesteps that entirely). Multiple mountpoints
//! per device (e.g. a btrfs subvolume mounted at several paths) are
//! listed one per extra line under the device, matching real
//! `lsblk`'s own multi-line layout.
//!
//! `SIZE` is rendered as a binary-prefix (K/M/G/T, powers of 1024)
//! human-readable string, matching real `lsblk`'s default style but
//! not necessarily its exact rounding — the same kind of cosmetic gap
//! already documented for `free -h` elsewhere in this crate.
//!
//! Verified against real `lsblk`/`lsblk -f`'s device tree (not the
//! `-f` filesystem columns, which need `libblkid`, out of scope here):
//! same devices, same parent/partition tree structure and drawing,
//! same MAJ:MIN, matching SIZE within expected human-readable
//! rounding, and the same real mountpoints (including a multi-mounted
//! btrfs subvolume showing on separate lines the same way real
//! `lsblk` does).
//!
//! Not implemented: `-f` (filesystem type/UUID/label — needs
//! `libblkid`), `-o` (custom columns), `-J`/`-P` (JSON/pairs output),
//! loop/device-mapper/LVM/RAID relationship trees beyond simple
//! partitions.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::vec::IntoIter;

fn read_trimmed(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn read_u64(path: &Path) -> Option<u64> {
    read_trimmed(path)?.parse().ok()
}

fn human_size(sectors: u64) -> String {
    let bytes = sectors * 512;
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}{}", UNITS[0])
    } else if size.fract() == 0.0 {
        format!("{size:.0}{}", UNITS[unit])
    } else {
        format!("{size:.1}{}", UNITS[unit])
    }
}

fn device_type(name: &str, is_partition: bool) -> &'static str {
    if is_partition {
        return "part";
    }
    if name.starts_with("loop") {
        "loop"
    } else if name.starts_with("sr") {
        "rom"
    } else {
        "disk"
    }
}

/// Two lookup keys are needed, not one, because no single field in
/// `/proc/self/mountinfo` reliably identifies the real block device
/// for every filesystem:
/// - The trailing mount-source field (after the `-` separator) is
///   normally the real device path (`/dev/nvme0n1p2`) — except for
///   the root filesystem, which the kernel reports there as the
///   synthetic `/dev/root` alias instead of whatever `root=` path
///   was actually used at boot (confirmed directly via the real boot
///   test: `root=/dev/vda` still shows up as source `/dev/root`).
/// - The `MAJ:MIN` field (3rd column) is real for most filesystems
///   (and correctly resolves the `/dev/root` case above, since ext4/
///   vfat/etc. report the actual underlying device's number there)
///   — except btrfs, which reports its own internal anon-block-device
///   number for multi-subvolume mounts, *not* the real device's
///   number (confirmed directly on this dev machine: a btrfs
///   filesystem's `/home`/`/var/cache`/`/var/log`/`/` subvolumes all
///   share one `MAJ:MIN` that matches nothing in `/sys/block`, while
///   their mount-source field correctly says `/dev/nvme0n1p2`).
///
/// So: try the real device-path key first, and only fall back to
/// `MAJ:MIN` when the source is the `/dev/root` alias (or missing).
/// Real `lsblk` handles this the same way, via `libmount`.
fn mount_map() -> (HashMap<String, Vec<String>>, HashMap<String, Vec<String>>) {
    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
    let mut by_maj_min: HashMap<String, Vec<String>> = HashMap::new();
    let Ok(contents) = fs::read_to_string("/proc/self/mountinfo") else {
        return (by_name, by_maj_min);
    };
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let Some(maj_min) = fields.nth(2) else {
            continue;
        };
        let Some(mountpoint) = fields.nth(1) else {
            continue;
        };
        // Skip the optional fields (zero or more) up to the `-`
        // separator, then the filesystem type, to reach the source.
        let rest: Vec<&str> = fields.collect();
        let Some(dash_pos) = rest.iter().position(|f| *f == "-") else {
            continue;
        };
        let Some(source) = rest.get(dash_pos + 2) else {
            continue;
        };
        by_maj_min
            .entry(maj_min.to_string())
            .or_default()
            .push(mountpoint.to_string());
        if let Some(name) = source.strip_prefix("/dev/").filter(|n| *n != "root") {
            by_name
                .entry(name.to_string())
                .or_default()
                .push(mountpoint.to_string());
        }
    }
    (by_name, by_maj_min)
}

struct Row {
    name: String,
    maj_min: String,
    rm: String,
    size: String,
    ro: String,
    kind: &'static str,
    mountpoints: Vec<String>,
    prefix: String,
}

fn build_row(
    dev_dir: &Path,
    name: &str,
    is_partition: bool,
    prefix: &str,
    mounts_by_name: &HashMap<String, Vec<String>>,
    mounts_by_maj_min: &HashMap<String, Vec<String>>,
) -> Row {
    let maj_min = read_trimmed(&dev_dir.join("dev")).unwrap_or_else(|| "?:?".to_string());
    let sectors = read_u64(&dev_dir.join("size")).unwrap_or(0);
    let ro = read_trimmed(&dev_dir.join("ro")).unwrap_or_else(|| "0".to_string());
    let rm = read_trimmed(&dev_dir.join("removable")).unwrap_or_else(|| "0".to_string());
    let mountpoints = mounts_by_name
        .get(name)
        .or_else(|| mounts_by_maj_min.get(&maj_min))
        .cloned()
        .unwrap_or_default();
    Row {
        name: name.to_string(),
        maj_min,
        rm,
        size: human_size(sectors),
        ro,
        kind: device_type(name, is_partition),
        mountpoints,
        prefix: prefix.to_string(),
    }
}

pub fn run(_args: IntoIter<OsString>) -> i32 {
    let (mounts_by_name, mounts_by_maj_min) = mount_map();
    let Ok(entries) = fs::read_dir("/sys/block") else {
        eprintln!("lsblk: cannot read /sys/block");
        return 1;
    };
    let mut disk_names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    disk_names.sort();

    let mut rows = Vec::new();
    for disk in &disk_names {
        let disk_dir = Path::new("/sys/block").join(disk);
        // Real lsblk inherits the parent disk's RM flag for
        // partitions, since partition directories don't have their
        // own `removable` file.
        let disk_rm = read_trimmed(&disk_dir.join("removable")).unwrap_or_else(|| "0".to_string());
        rows.push(build_row(
            &disk_dir,
            disk,
            false,
            "",
            &mounts_by_name,
            &mounts_by_maj_min,
        ));

        let mut parts: Vec<String> = fs::read_dir(&disk_dir)
            .map(|it| {
                it.filter_map(|e| e.ok())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .filter(|n| n.starts_with(disk.as_str()) && n != disk)
                    .collect()
            })
            .unwrap_or_default();
        parts.sort();

        for (i, part) in parts.iter().enumerate() {
            let part_dir = disk_dir.join(part);
            let mut row = build_row(
                &part_dir,
                part,
                true,
                "",
                &mounts_by_name,
                &mounts_by_maj_min,
            );
            row.rm = disk_rm.clone();
            row.prefix = if i + 1 == parts.len() {
                "└─".to_string()
            } else {
                "├─".to_string()
            };
            rows.push(row);
        }
    }

    println!(
        "{:<11} {:<7} {:<2} {:>6} {:<2} {:<4} MOUNTPOINTS",
        "NAME", "MAJ:MIN", "RM", "SIZE", "RO", "TYPE"
    );
    for row in &rows {
        let display_name = format!("{}{}", row.prefix, row.name);
        let first_mount = row.mountpoints.first().map(String::as_str).unwrap_or("");
        println!(
            "{:<11} {:<7} {:<2} {:>6} {:<2} {:<4} {}",
            display_name, row.maj_min, row.rm, row.size, row.ro, row.kind, first_mount
        );
        for extra in row.mountpoints.iter().skip(1) {
            println!(
                "{:<11} {:<7} {:<2} {:>6} {:<2} {:<4} {}",
                "", "", "", "", "", "", extra
            );
        }
    }
    0
}
