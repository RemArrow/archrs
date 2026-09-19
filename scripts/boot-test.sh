#!/bin/sh
# Boots a real, minimal archrs system in QEMU and checks that it actually
# comes up: archrs-init as genuine PID 1, mounts proc/sys/dev, runs
# coreutils-rs's bash to exercise a handful of utilities, and shuts itself
# down via SIGUSR2 -> a real reboot(2) syscall. See ROADMAP.md's "Real boot
# test" section for what this is checking and why it matters (an unshare
# sandbox can't grant the privileges real mount(2)/reboot(2) calls need, so
# this is the only way to verify that code path for real).
#
# Needs no host root: pacman-rs installs into a plain directory, and
# `mke2fs -d` populates an ext4 image straight from that directory without
# ever mounting it. QEMU itself needs no privilege to boot a kernel image
# either. /dev/kvm is used opportunistically if writable; falls back to
# software emulation (slower, still correct) otherwise.
#
# Usage: scripts/boot-test.sh [--clean]
#   --clean   wipe the cached rootfs and reinstall the base system (network
#             access to real Arch/Manjaro mirrors required either way, the
#             first time)

set -eu

WORKSPACE_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="$WORKSPACE_ROOT/target/release"
WORKDIR="${ARCHRS_BOOT_TEST_DIR:-/tmp/archrs-boot-test}"
ROOTFS="$WORKDIR/rootfs"
IMAGE="$WORKDIR/rootfs.img"
LOG="$WORKDIR/boot.log"
KERNEL="${ARCHRS_BOOT_TEST_KERNEL:-}"

if [ "${1:-}" = "--clean" ]; then
    rm -rf "$WORKDIR"
fi

if [ -z "$KERNEL" ]; then
    KERNEL=$(ls /boot/vmlinuz-* 2>/dev/null | head -1)
fi
if [ -z "$KERNEL" ] || [ ! -r "$KERNEL" ]; then
    echo "boot-test: no readable kernel image found (set ARCHRS_BOOT_TEST_KERNEL)" >&2
    exit 1
fi

for bin in coreutils-rs pacman-rs archrs-init crond crontab; do
    if [ ! -x "$TARGET/$bin" ]; then
        echo "boot-test: $TARGET/$bin missing — run 'cargo build --release' first" >&2
        exit 1
    fi
done

mkdir -p "$ROOTFS"

if [ ! -f "$ROOTFS/.base-installed" ]; then
    echo "boot-test: installing base system into $ROOTFS (glibc, filesystem, bash, xz, file)..."
    "$TARGET/pacman-rs" -Sy --root "$ROOTFS"
    "$TARGET/pacman-rs" -S glibc filesystem bash xz file --root "$ROOTFS"
    touch "$ROOTFS/.base-installed"
fi

echo "boot-test: installing archrs binaries..."
mkdir -p "$ROOTFS/usr/local/bin"
for bin in coreutils-rs pacman-rs makepkg-rs archrs-init crond crontab; do
    cp "$TARGET/$bin" "$ROOTFS/usr/local/bin/$bin"
done
rm -f "$ROOTFS/sbin/init.archrs"
cp "$TARGET/archrs-init" "$ROOTFS/sbin/init.archrs"

# Symlink every utility coreutils-rs dispatches, straight from its own
# source of truth, so this list can never drift out of sync with it.
grep -oP '^\s*\("\K[^"]+' "$WORKSPACE_ROOT/crates/coreutils-rs/src/util_list.rs" | sort -u |
while IFS= read -r util; do
    ln -sf /usr/local/bin/coreutils-rs "$ROOTFS/usr/bin/$util"
done

cat > "$ROOTFS/root/boot-test.sh" <<'SCRIPT'
#!/usr/bin/sh
echo "ARCHRS-BOOT-TEST: starting"
echo "ARCHRS-BOOT-TEST: uname -a: $(uname -a)"
echo "ARCHRS-BOOT-TEST: id: $(id)"
ls /proc/1 >/dev/null 2>&1 && echo "ARCHRS-BOOT-TEST: /proc/1 accessible: PASS" || echo "ARCHRS-BOOT-TEST: /proc/1 accessible: FAIL"
echo "ARCHRS-BOOT-TEST: ps aux:"
ps aux
echo "ARCHRS-BOOT-TEST: free:"
free
echo "ARCHRS-BOOT-TEST: awk result: $(echo "1 2 3" | awk '{ print $2 }')"
echo "ARCHRS-BOOT-TEST: arithmetic result: $((6*7))"
echo "hello from archrs vm" > /tmp/marker.txt
echo "ARCHRS-BOOT-TEST: file roundtrip: $(cat /tmp/marker.txt)"
echo "ARCHRS-BOOT-TEST: diff result: $(printf 'a\nb\n' > /tmp/d1.txt; printf 'a\nc\n' > /tmp/d2.txt; diff /tmp/d1.txt /tmp/d2.txt | tr '\n' '|')"
echo "ARCHRS-BOOT-TEST: dmesg first line: $(dmesg | head -1)"
echo "ARCHRS-BOOT-TEST: all checks complete, powering off"
kill -USR2 1
sleep 5
echo "ARCHRS-BOOT-TEST: FAIL: still alive after poweroff signal"
SCRIPT
chmod +x "$ROOTFS/root/boot-test.sh"
echo "/bin/sh /root/boot-test.sh" > "$ROOTFS/etc/archrs-init.conf"

echo "boot-test: building disk image..."
rm -f "$IMAGE"
truncate -s 1G "$IMAGE"
mke2fs -F -t ext4 -d "$ROOTFS" -L archrsroot "$IMAGE" >/dev/null

KVM_ARGS=""
if [ -w /dev/kvm ]; then
    KVM_ARGS="-enable-kvm"
fi

echo "boot-test: booting..."
timeout 60 qemu-system-x86_64 \
    -kernel "$KERNEL" \
    -drive file="$IMAGE",format=raw,if=virtio \
    -append "root=/dev/vda rw console=ttyS0 init=/sbin/init.archrs panic=1" \
    -m 1G \
    -nographic \
    -no-reboot \
    -net none \
    $KVM_ARGS \
    > "$LOG" 2>&1 || true

echo "boot-test: full log at $LOG"
echo "---"

fail=0
check() {
    if grep -qF "$1" "$LOG"; then
        echo "PASS: $1"
    else
        echo "FAIL: $1 (not found in log)"
        fail=1
    fi
}

check "archrs-init: mounted proc on /proc"
check "archrs-init: mounted sysfs on /sys"
check "ARCHRS-BOOT-TEST: /proc/1 accessible: PASS"
check "ARCHRS-BOOT-TEST: arithmetic result: 42"
check "ARCHRS-BOOT-TEST: file roundtrip: hello from archrs vm"
check "ARCHRS-BOOT-TEST: awk result: 2"
check "ARCHRS-BOOT-TEST: diff result: 2c2|< b|---|> c|"
check "ARCHRS-BOOT-TEST: dmesg first line: [    0.000000] Linux version"
check "archrs-init: powering off"
check "reboot: Power down"

if [ "$fail" -eq 0 ]; then
    echo "---"
    echo "boot-test: ALL CHECKS PASSED"
else
    echo "---"
    echo "boot-test: SOME CHECKS FAILED — see $LOG"
    exit 1
fi
