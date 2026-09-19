//! `lsusb` (usbutils) — lists USB devices, resolving vendor/product
//! IDs to human-readable names via the `usb-ids` crate, which embeds
//! the real USB ID Repository database (`usb.ids`) at compile time —
//! same reasoning as `lspci_cmd.rs` vendoring `pci-ids`: this is what
//! actually needs a database, unlike most of Phase 5/6/7's direct
//! `/proc`/`/sys` reads.
//!
//! Device enumeration is a direct `/sys/bus/usb/devices/*` read
//! (`busnum`/`devnum`/`idVendor`/`idProduct`), skipping entries whose
//! name contains `:` (those are USB *interfaces*, e.g. `1-10:1.0`,
//! not top-level devices — real `lsusb` only lists devices by
//! default). Root hubs (`usb1`, `usb2`, ...) need no special-casing:
//! the kernel exposes them as ordinary USB devices with a real
//! (Linux Foundation-assigned) vendor/product ID, so they fall out of
//! the same generic path as every other device.
//!
//! Scope: default (no-arguments) output only, one line per device:
//! `Bus BBB Device DDD: ID vvvv:pppp VendorName ProductName`.
//!
//! Verified against real `lsusb` on this dev machine: every device's
//! bus/device number and vendor:product ID matched exactly, including
//! the USB2/USB3 root hubs themselves (`1d6b:0002`/`1d6b:0003`, Linux
//! Foundation). Name resolution matched for most devices but not all:
//! two real devices here (a Wacom touch sensor, an IMC Networks
//! camera) resolved their *vendor* name correctly but not their
//! specific *product* ID — confirmed, by querying the vendored
//! database directly, that this is a real database-currency gap (the
//! `usb-ids` crate's snapshot doesn't have that exact product ID
//! yet), not a lookup bug, since the same vendor ID resolves
//! correctly on its own. Falls back to vendor-name-only rather than
//! blank in that case.
//!
//! Not implemented: `-v` (verbose: descriptors, configurations),
//! `-t` (tree view), `-d` (filter by vendor:product), USB device tree
//! parent/child relationships.

use std::ffi::OsString;
use std::fs;
use std::vec::IntoIter;

fn read_trimmed(path: &str) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

fn read_hex_u16(path: &str) -> Option<u16> {
    u16::from_str_radix(&read_trimmed(path)?, 16).ok()
}

pub fn run(_args: IntoIter<OsString>) -> i32 {
    let Ok(entries) = fs::read_dir("/sys/bus/usb/devices") else {
        eprintln!("lsusb: cannot read /sys/bus/usb/devices");
        return 1;
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| !n.contains(':'))
        .collect();
    names.sort();

    let mut rows: Vec<(u32, u32, u16, u16)> = Vec::new();
    for name in &names {
        let base = format!("/sys/bus/usb/devices/{name}");
        let Some(bus) = read_trimmed(&format!("{base}/busnum")).and_then(|s| s.parse().ok()) else {
            continue;
        };
        let Some(dev) = read_trimmed(&format!("{base}/devnum")).and_then(|s| s.parse().ok()) else {
            continue;
        };
        let Some(vendor) = read_hex_u16(&format!("{base}/idVendor")) else {
            continue;
        };
        let Some(product) = read_hex_u16(&format!("{base}/idProduct")) else {
            continue;
        };
        rows.push((bus, dev, vendor, product));
    }
    // Real lsusb sorts by bus number, then device number.
    rows.sort_by_key(|&(bus, dev, _, _)| (bus, dev));

    for (bus, dev, vendor, product) in rows {
        let (vendor_name, product_name) = match usb_ids::Device::from_vid_pid(vendor, product) {
            Some(d) => (d.vendor().name().to_string(), d.name().to_string()),
            // The specific vendor:product pair isn't in the vendored
            // usb.ids snapshot (a database-currency gap, not a logic
            // bug — confirmed directly: some real vendor IDs resolve
            // even when their specific product ID doesn't). Fall back
            // to the vendor name alone rather than showing nothing.
            None => (
                usb_ids::Vendors::iter()
                    .find(|v| v.id() == vendor)
                    .map(|v| v.name().to_string())
                    .unwrap_or_default(),
                String::new(),
            ),
        };
        println!(
            "Bus {bus:03} Device {dev:03}: ID {vendor:04x}:{product:04x} {vendor_name} {product_name}"
        );
    }
    0
}
