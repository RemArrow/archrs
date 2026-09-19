#!/bin/sh
# Boots a real, minimal archrs system in QEMU and checks that it actually
# comes up: archrs-init as genuine PID 1, mounts proc/sys/dev, runs
# coreutils-rs's bash to exercise a handful of utilities, and shuts itself
# down via `poweroff` -> SIGUSR2 -> a real reboot(2) syscall. See
# ROADMAP.md's "Real boot test" section for what this is checking and why
# it matters (an unshare sandbox can't grant the privileges real
# mount(2)/reboot(2) calls need, so this is the only way to verify that
# code path for real). For a real *interactive* login session instead of
# this fixed script, see scripts/login-test.sh.
#
# Needs no host root: pacman-rs installs into a plain directory, and
# `mke2fs -d` populates an ext4 image straight from that directory without
# ever mounting it. QEMU itself needs no privilege to boot a kernel image
# either. /dev/kvm is used opportunistically if writable; falls back to
# software emulation (slower, still correct) otherwise.
#
# Image contents are, however, built under `fakeroot` when it's available
# (opportunistically, like /dev/kvm above — falls back to every file
# landing owned by the host's own uid if it isn't installed). This is what
# makes the image's files genuinely root-owned, and the su/sudo/visudo
# binaries genuinely setuid-root, without ever needing real host root:
# fakeroot intercepts chown/chmod/stat within the wrapped commands and
# fakes their results consistently, and since alpm-rs now actually
# attempts to preserve each extracted file's own embedded archive
# ownership (see crates/alpm-rs/src/install.rs's try_preserve_ownership),
# those faked chowns really happen during package install, and the same
# fakeroot session's later `mke2fs -d` call reads them back and bakes them
# into the real ext4 image — verified directly with `debugfs -R stat` on a
# scratch image before wiring this in. Without fakeroot, every file is
# host-uid-owned as before (see the Phase 8 note in ROADMAP.md on why
# `sudo` specifically needs this to actually work end to end).
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
FAKEROOT_STATE="$WORKDIR/fakeroot.state"

if command -v fakeroot >/dev/null 2>&1; then
    FAKEROOT="fakeroot -i $FAKEROOT_STATE -s $FAKEROOT_STATE --"
else
    echo "boot-test: fakeroot not installed — image files will be host-uid-owned, not root-owned (sudo's own hardening will correctly refuse rather than run; see ROADMAP.md's Phase 8 section)" >&2
    FAKEROOT=""
fi

if [ "${1:-}" = "--clean" ]; then
    rm -rf "$WORKDIR"
fi

. "$WORKSPACE_ROOT/scripts/lib-build-rootfs.sh"

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
echo "ARCHRS-BOOT-TEST: hostname: $(hostname)"
echo "ARCHRS-BOOT-TEST: chroot result: $(chroot / hostname 2>&1)"
chroot / true
echo "ARCHRS-BOOT-TEST: chroot exit code: $?"
mkdir -p /mnt/tmpfstest
mount tmpfs /mnt/tmpfstest -t tmpfs -o size=1m
echo "ARCHRS-BOOT-TEST: tmpfs mounted: $(mount | grep tmpfstest)"
echo "written through tmpfs" > /mnt/tmpfstest/probe.txt
echo "ARCHRS-BOOT-TEST: tmpfs write: $(cat /mnt/tmpfstest/probe.txt)"
umount /mnt/tmpfstest
echo "ARCHRS-BOOT-TEST: tmpfs unmounted: $(mount | grep -c tmpfstest)"
echo "ARCHRS-BOOT-TEST: ss listener: $(ss -tl | grep -c LISTEN || true)"
echo "ARCHRS-BOOT-TEST: ip addr:"
ip addr | tr '\n' '|'
echo
echo "ARCHRS-BOOT-TEST: lsblk:"
lsblk | tr '\n' '|'
echo
echo "ARCHRS-BOOT-TEST: lspci exit code: $(lspci >/dev/null 2>&1; echo $?)"
echo "ARCHRS-BOOT-TEST: lsusb exit code: $(lsusb >/dev/null 2>&1; echo $?)"
useradd -m -s /bin/sh testuser
echo "ARCHRS-BOOT-TEST: useradd exit code: $?"
echo "testuser:secret123" | chpasswd
echo "ARCHRS-BOOT-TEST: chpasswd exit code: $?"
echo "ARCHRS-BOOT-TEST: shadow hash: $(grep -c '^testuser:\$' /etc/shadow)"
echo "ARCHRS-BOOT-TEST: su result: $(su - testuser -c 'id -un')"
# sudo-rs hardens further than su-rs: it also demands /etc (and every
# ancestor of /etc/sudoers) be genuinely root-owned, not just its own
# binary — correctly refuses otherwise, rather than trusting a directory
# a non-root user could tamper with. This is why the image build is
# wrapped in $FAKEROOT (see this script's own top-of-file comment and
# ROADMAP.md's Phase 8 section): without it, every file including /etc
# would be host-uid-owned and this would genuinely, correctly fail.
echo "ARCHRS-BOOT-TEST: sudo result: $(sudo -u testuser id -un 2>&1)"
echo "ARCHRS-BOOT-TEST: lsmod exit code: $(lsmod >/dev/null 2>&1; echo $?)"
echo "ARCHRS-BOOT-TEST: rmmod nonexistent: $(rmmod not_a_real_module 2>&1)"
echo "ARCHRS-BOOT-TEST: modprobe nonexistent: $(modprobe not_a_real_module 2>&1)"
# A non-root user must not be able to power the machine off — kill(2)'s
# own real permission check (sender's uid must match PID 1's, i.e. be
# root) does this for free, no separate check needed in poweroff_cmd.rs.
echo "ARCHRS-BOOT-TEST: unprivileged poweroff: $(su - testuser -c poweroff 2>&1)"
echo "ARCHRS-BOOT-TEST: all checks complete, powering off"
poweroff
sleep 5
echo "ARCHRS-BOOT-TEST: FAIL: still alive after poweroff"
SCRIPT
chmod +x "$ROOTFS/root/boot-test.sh"
echo "/bin/sh /root/boot-test.sh" > "$ROOTFS/etc/archrs-init.conf"

echo "boot-test: building disk image..."
rm -f "$IMAGE"
truncate -s 1G "$IMAGE"
$FAKEROOT mke2fs -F -t ext4 -d "$ROOTFS" -L archrsroot "$IMAGE" >/dev/null

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
check "ARCHRS-BOOT-TEST: chroot exit code: 0"
check "ARCHRS-BOOT-TEST: tmpfs write: written through tmpfs"
check "ARCHRS-BOOT-TEST: tmpfs unmounted: 0"
check "1: lo: <LOOPBACK>"
check "vda         254:0"
check "1G 0  disk /|"
check "ARCHRS-BOOT-TEST: lspci exit code: 0"
check "ARCHRS-BOOT-TEST: lsusb exit code: 0"
check "ARCHRS-BOOT-TEST: useradd exit code: 0"
check "ARCHRS-BOOT-TEST: chpasswd exit code: 0"
check "ARCHRS-BOOT-TEST: shadow hash: 1"
check "ARCHRS-BOOT-TEST: su result: testuser"
if [ -n "$FAKEROOT" ]; then
    check "ARCHRS-BOOT-TEST: sudo result: testuser"
else
    check "ARCHRS-BOOT-TEST: sudo result: sudo: invalid configuration: /etc must be owned by root"
fi
check "ARCHRS-BOOT-TEST: lsmod exit code: 0"
check "ARCHRS-BOOT-TEST: rmmod nonexistent: libkmod: ERROR: kmod_module_remove_module: could not remove 'not_a_real_module': No such file or directory"
check "ARCHRS-BOOT-TEST: modprobe nonexistent: modprobe: module 'not_a_real_module' not found"
check "ARCHRS-BOOT-TEST: unprivileged poweroff: poweroff: could not signal PID 1:"
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
