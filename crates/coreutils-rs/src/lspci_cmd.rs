//! `lspci` (pciutils) — lists PCI devices, resolving vendor/device/
//! class IDs to human-readable names via the `pci-ids` crate, which
//! embeds the real PCI ID Repository database (`pci.ids`) at compile
//! time — the same database real `lspci` itself reads (usually from
//! `/usr/share/hwdata/pci.ids` on Arch), just vendored as compiled-in
//! Rust data instead of depending on that file being installed on the
//! target system. This is what actually distinguishes `lspci`/`lsusb`
//! from the rest of Phase 5/6/7: `lsblk`, `ss`, `ip`, `dmesg`, etc. all
//! only needed direct `/proc`/`/sys` reads, but resolving a PCI vendor
//! ID to a name genuinely needs a database, not just a kernel
//! interface.
//!
//! Device enumeration itself is a direct `/sys/bus/pci/devices/*`
//! read (`vendor`/`device`/`class`/`revision` attribute files) — no
//! crate needed for that part, same as `lsblk`.
//!
//! Scope: default (no-arguments) output only, one line per device:
//! `BUS:SLOT.FUNC ClassName: VendorName DeviceName (rev NN)` — domain
//! prefix (`DDDD:`) is dropped for the default `0000` domain, matching
//! real `lspci`'s own default-domain elision, and kept for any other
//! domain (rare, multi-domain systems).
//!
//! Verified against real `lspci` on this dev machine: every device's
//! bus address, class name, vendor name, and device name matched
//! exactly (Intel host bridge/GPU/thermal/etc. entries, all resolved
//! from `pci-ids`' embedded database), including revision numbers.
//!
//! Not implemented: `-v`/`-vv` (verbose: capabilities, BARs, kernel
//! driver), `-n`/`-nn` (numeric IDs), `-k` (kernel driver in use),
//! filtering by bus/device/vendor.

use std::ffi::OsString;
use std::fs;
use std::vec::IntoIter;

fn read_hex(path: &str) -> Option<u32> {
    let raw = fs::read_to_string(path).ok()?;
    let trimmed = raw.trim().trim_start_matches("0x");
    u32::from_str_radix(trimmed, 16).ok()
}

fn class_name(class: u32) -> String {
    let base = ((class >> 16) & 0xff) as u8;
    let sub = ((class >> 8) & 0xff) as u8;
    if let Some(subclass) = pci_ids::Subclass::from_cid_sid(base, sub) {
        return subclass.name().to_string();
    }
    if let Some(class) = pci_ids::Classes::iter().find(|c| c.id() == base) {
        return class.name().to_string();
    }
    format!("Class {class:06x}")
}

pub fn run(_args: IntoIter<OsString>) -> i32 {
    let Ok(entries) = fs::read_dir("/sys/bus/pci/devices") else {
        eprintln!("lspci: cannot read /sys/bus/pci/devices");
        return 1;
    };
    let mut addrs: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    addrs.sort();

    for addr in &addrs {
        let base = format!("/sys/bus/pci/devices/{addr}");
        let Some(vendor_id) = read_hex(&format!("{base}/vendor")) else {
            continue;
        };
        let Some(device_id) = read_hex(&format!("{base}/device")) else {
            continue;
        };
        let class = read_hex(&format!("{base}/class")).unwrap_or(0);
        let revision = read_hex(&format!("{base}/revision")).unwrap_or(0);

        let (vendor_name, device_name) =
            match pci_ids::Device::from_vid_pid(vendor_id as u16, device_id as u16) {
                Some(dev) => (dev.vendor().name().to_string(), dev.name().to_string()),
                None => (
                    format!("Vendor {vendor_id:04x}"),
                    format!("Device {device_id:04x}"),
                ),
            };

        // Sysfs directory names are always full "DDDD:BB:SS.F"; real
        // lspci drops the domain when it's the default 0000.
        let display_addr = addr
            .strip_prefix("0000:")
            .map(str::to_string)
            .unwrap_or_else(|| addr.clone());

        let class = class_name(class);
        // Real lspci omits the "(rev NN)" suffix entirely when the
        // revision is 0 (its "nothing meaningful to report" value),
        // confirmed against this dev machine's own NVMe controller.
        if revision == 0 {
            println!("{display_addr} {class}: {vendor_name} {device_name}");
        } else {
            println!("{display_addr} {class}: {vendor_name} {device_name} (rev {revision:02x})");
        }
    }
    0
}
