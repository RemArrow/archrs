//! Every real safety check for `--target DEVICE`, the one genuinely
//! destructive path in this whole tool — see `main.rs`'s own doc
//! comment on why every other step here stays root-free and file-based
//! until this exact point. Deliberately conservative: refuses rather
//! than guesses whenever something can't be determined confidently.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

fn device_basename(dev: &Path) -> Result<String> {
    dev.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .with_context(|| format!("{dev:?} has no file name"))
}

pub fn device_size_bytes(dev: &Path) -> Result<u64> {
    let name = device_basename(dev)?;
    let size_path = format!("/sys/class/block/{name}/size");
    let sectors: u64 = std::fs::read_to_string(&size_path)
        .with_context(|| {
            format!("{dev:?} doesn't look like a real block device ({size_path} not found)")
        })?
        .trim()
        .parse()
        .with_context(|| format!("parsing {size_path}"))?;
    Ok(sectors * 512)
}

/// True if `dev` or anything that looks like one of its own partitions
/// (`dev` + a trailing digit, or `dev` + `pN` for an nvme-style name)
/// appears as a mount source in `/proc/mounts`.
fn device_or_partition_is_mounted(dev: &Path) -> Result<bool> {
    let dev_str = dev.to_string_lossy();
    let mounts = std::fs::read_to_string("/proc/mounts").context("reading /proc/mounts")?;
    for line in mounts.lines() {
        let Some(source) = line.split_whitespace().next() else {
            continue;
        };
        if let Some(rest) = source.strip_prefix(dev_str.as_ref())
            && (rest.is_empty()
                || rest.chars().all(|c| c.is_ascii_digit())
                || rest.starts_with('p'))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Resolves the real disk backing the currently-mounted `/`, by
/// reading `/proc/mounts`'s own source field for the root mount and
/// stripping a trailing partition-number suffix — refuses to guess
/// past that (e.g. LVM/device-mapper roots resolve to a `/dev/mapper/*`
/// name this can't trace further; treated as "can't determine", which
/// still doesn't block a real `--target` that's obviously different).
fn running_system_disk() -> Result<Option<PathBuf>> {
    let mounts = std::fs::read_to_string("/proc/mounts").context("reading /proc/mounts")?;
    for line in mounts.lines() {
        let mut fields = line.split_whitespace();
        let Some(source) = fields.next() else {
            continue;
        };
        let Some(mountpoint) = fields.next() else {
            continue;
        };
        if mountpoint == "/" {
            let trimmed = source.trim_end_matches(|c: char| c.is_ascii_digit());
            let trimmed = trimmed.strip_suffix('p').unwrap_or(trimmed);
            return Ok(Some(PathBuf::from(trimmed)));
        }
    }
    Ok(None)
}

pub fn run_safety_checks(target: &Path) -> Result<()> {
    if !target.exists() {
        bail!("{target:?} does not exist");
    }
    if device_or_partition_is_mounted(target)? {
        bail!(
            "{target:?} (or one of its partitions) is currently mounted — refusing to touch a \
             mounted device"
        );
    }
    if let Some(root_disk) = running_system_disk()?
        && root_disk == target
    {
        bail!(
            "{target:?} appears to be the disk backing the currently-running system's own root \
             filesystem — refusing"
        );
    }
    Ok(())
}

/// Prints the real "this will erase everything" warning and, unless
/// `yes_i_am_sure`, requires typing the exact device path back —
/// called *before* `device::partition` touches anything, since
/// partitioning itself is the first real, destructive step (there's no
/// separate "build then dd" phase to gate here anymore — see `main.rs`'s
/// own doc comment on why).
pub fn confirm(target: &Path, yes_i_am_sure: bool) -> Result<()> {
    let size = device_size_bytes(target)?;
    println!(
        "\n!!! This will PERMANENTLY ERASE {target:?} ({:.1} GiB) !!!",
        size as f64 / (1024.0 * 1024.0 * 1024.0)
    );

    if yes_i_am_sure {
        return Ok(());
    }
    print!(
        "Type the exact device path ({}) to confirm: ",
        target.display()
    );
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    if line.trim() != target.to_string_lossy() {
        bail!("confirmation did not match — aborting, nothing was written");
    }
    Ok(())
}
