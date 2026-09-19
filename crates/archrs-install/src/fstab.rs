//! Writes `/etc/fstab`, `/etc/hostname`, `/etc/hosts` — small, real
//! files a genuinely bootable system needs that a throwaway QEMU test
//! image never did (those never reboot into a persisted `/etc/fstab`;
//! this one has to mount its own root and ESP correctly on every real
//! boot).

use std::path::Path;

use anyhow::Result;

/// `PARTUUID=` entries, not device paths or filesystem `UUID=` — real
/// GPT partition GUIDs `device::partition` reads back from `blkid`
/// after `parted` assigns them, and PARTUUID resolution is a kernel
/// built-in (`name_to_dev_t`/`devt_from_partuuid`), not a udev
/// `/dev/disk/by-*` symlink lookup this project has nothing to
/// populate yet.
pub fn write(rootfs_dir: &Path, root_guid: uuid::Uuid, esp_guid: uuid::Uuid) -> Result<()> {
    let fstab = format!(
        "PARTUUID={root_guid}  /      ext4  rw,relatime  0 1\n\
         PARTUUID={esp_guid}   /boot  vfat  rw,relatime  0 2\n"
    );
    std::fs::write(rootfs_dir.join("etc/fstab"), fstab)?;
    std::fs::write(rootfs_dir.join("etc/hostname"), "archrs\n")?;
    std::fs::write(
        rootfs_dir.join("etc/hosts"),
        "127.0.0.1\tlocalhost\n::1\t\tlocalhost\n127.0.1.1\tarchrs.localdomain\tarchrs\n",
    )?;
    Ok(())
}
