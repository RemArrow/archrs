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

for bin in coreutils-rs pacman-rs archrs-init crond crontab su sudo visudo; do
    if [ ! -x "$TARGET/$bin" ]; then
        echo "boot-test: $TARGET/$bin missing — run 'cargo build --release' first" >&2
        exit 1
    fi
done

mkdir -p "$ROOTFS"

if [ ! -f "$ROOTFS/.base-installed" ]; then
    # pam/sudo/shadow/util-linux pulled in purely for what this project
    # doesn't reimplement: libpam.so + its real pam_unix/pam_rootok/...
    # modules, and the real /etc/pam.d/{su,sudo,system-auth}, /etc/sudoers,
    # /etc/login.defs config files (Arch's `shadow` package ships no su/
    # login at all — those come from util-linux, which is also the only
    # source of /etc/pam.d/su's `auth sufficient pam_rootok.so` line, the
    # real mechanism that lets root su/sudo without a password). Every
    # binary any of these four packages ship gets immediately overwritten
    # below by this project's own coreutils-rs/privtools-rs equivalents
    # where one exists — only the supporting files are actually used from
    # them, same pattern as glibc/filesystem/bash/xz/file already below.
    echo "boot-test: installing base system into $ROOTFS (glibc, filesystem, bash, xz, file, pam, sudo, shadow, util-linux, kmod)..."
    "$TARGET/pacman-rs" -Sy --root "$ROOTFS"
    "$TARGET/pacman-rs" -S glibc filesystem bash xz file pam sudo shadow util-linux kmod --root "$ROOTFS"
    touch "$ROOTFS/.base-installed"
fi

echo "boot-test: installing archrs binaries..."
mkdir -p "$ROOTFS/usr/local/bin"
for bin in coreutils-rs pacman-rs makepkg-rs archrs-init crond crontab; do
    cp "$TARGET/$bin" "$ROOTFS/usr/local/bin/$bin"
done
rm -f "$ROOTFS/sbin/init.archrs"
cp "$TARGET/archrs-init" "$ROOTFS/sbin/init.archrs"

# su/sudo/visudo: real standalone setuid-root binaries in real distros —
# deliberately NOT symlinked through coreutils-rs's multicall dispatch
# (see crates/privtools-rs's doc comment). Installed straight to /usr/bin,
# overwriting the real util-linux/sudo package's own binaries but keeping
# their PAM/sudoers config files. Actual setuid-root ownership can't be
# set here (this whole image build deliberately runs without host root,
# so every file lands owned by the host's own uid, not 0) — not needed for
# what this test verifies, since every process in the booted VM already
# runs as genuine root; a real non-root user gaining privileges through
# these binaries on a real install still needs a real `chown root:root` +
# `chmod u+s` step outside this pipeline, left open like the rest of
# "distributable" packaging.
for bin in su sudo visudo; do
    cp "$TARGET/$bin" "$ROOTFS/usr/bin/$bin"
done

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
# binary. This image is built entirely without host root (see the
# install step's comment), so every file — including /etc itself —
# is owned by the host's real uid, not 0; sudo-rs correctly detects
# and refuses this rather than trusting a directory a non-root user
# could tamper with. Checking for that exact refusal message verifies
# the hardening logic fires for real, which is what's actually
# reachable here; sudo's full functional path needs a real- or
# fake-rooted image build, deliberately not done yet (see ROADMAP.md).
echo "ARCHRS-BOOT-TEST: sudo refusal: $(sudo -u testuser id -un 2>&1)"
echo "ARCHRS-BOOT-TEST: lsmod exit code: $(lsmod >/dev/null 2>&1; echo $?)"
echo "ARCHRS-BOOT-TEST: rmmod nonexistent: $(rmmod not_a_real_module 2>&1)"
echo "ARCHRS-BOOT-TEST: modprobe nonexistent: $(modprobe not_a_real_module 2>&1)"
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
check "ARCHRS-BOOT-TEST: sudo refusal: sudo: invalid configuration: /etc must be owned by root"
check "ARCHRS-BOOT-TEST: lsmod exit code: 0"
check "ARCHRS-BOOT-TEST: rmmod nonexistent: libkmod: ERROR: kmod_module_remove_module: could not remove 'not_a_real_module': No such file or directory"
check "ARCHRS-BOOT-TEST: modprobe nonexistent: modprobe: module 'not_a_real_module' not found"
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
