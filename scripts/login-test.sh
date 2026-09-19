#!/bin/sh
# Boots the same real archrs image as scripts/boot-test.sh, but drives a
# genuine interactive login session over QEMU's serial console instead of
# running a fixed script: a real `agetty` respawning on ttyS0, real
# `login(1)` authenticating against real PAM/shadow, a wrong password
# actually getting rejected, a correct one actually succeeding and
# exec'ing a real shell as the real user, an unprivileged `poweroff`
# actually being refused, and logging out actually bringing agetty back.
# This is the difference between "boots and runs a script" and "a human
# could sit down at this and use it" — see scripts/login-test-driver.py
# for the actual interaction.
#
# Needs python3 (for the driver) in addition to everything
# scripts/boot-test.sh needs; shares its rootfs-build logic (and, by
# default, its cached rootfs/downloaded packages) via
# scripts/lib-build-rootfs.sh.
#
# Usage: scripts/login-test.sh [--clean]

set -eu

WORKSPACE_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="$WORKSPACE_ROOT/target/release"
WORKDIR="${ARCHRS_BOOT_TEST_DIR:-/tmp/archrs-boot-test}"
ROOTFS="$WORKDIR/rootfs"
IMAGE="$WORKDIR/rootfs-login.img"
LOG="$WORKDIR/login-test.log"
SOCK="$WORKDIR/login-test.sock"
KERNEL="${ARCHRS_BOOT_TEST_KERNEL:-}"
FAKEROOT_STATE="$WORKDIR/fakeroot.state"

if ! command -v python3 >/dev/null 2>&1; then
    echo "login-test: python3 required (drives the interactive session)" >&2
    exit 1
fi

if command -v fakeroot >/dev/null 2>&1; then
    FAKEROOT="fakeroot -i $FAKEROOT_STATE -s $FAKEROOT_STATE --"
else
    echo "login-test: fakeroot not installed — su/sudo won't be genuinely setuid-root; login itself still works the same way it did before fakeroot support existed" >&2
    FAKEROOT=""
fi

if [ "${1:-}" = "--clean" ]; then
    rm -rf "$WORKDIR"
fi

. "$WORKSPACE_ROOT/scripts/lib-build-rootfs.sh"

# archrs-init's own command parser is a plain whitespace split with no
# shell-style quoting (see its own module doc comment — no IPC/parsing
# richness has been needed before now), so a `wait sh -c "a; b"` config
# line doesn't do what it looks like it does: the quotes are just
# characters to it, and "useradd/-m/-s/testuser/..." all land as
# separate argv entries to `sh -c`, which then only sees `-c` at its
# first non-flag argument. Confirmed for real (a genuine bug, not a
# guess): `useradd -m -s /bin/sh testuser 2>/dev/null; ...` inline in the
# config produced `error: unexpected argument '-m' found` from brush.
# Sidestepped the same way scripts/boot-test.sh's own fixed script
# already does: a real script *file*, invoked with two plain
# space-separated tokens (interpreter, path) that need no quoting at all.
cat > "$ROOTFS/root/setup-testuser.sh" <<'SETUP'
#!/bin/sh
useradd -m -s /bin/sh testuser 2>/dev/null
echo testuser:secret123 | chpasswd
SETUP
chmod +x "$ROOTFS/root/setup-testuser.sh"

# `-L` (ignore modem control lines): QEMU's virtual serial port never
# raises DCD, and agetty otherwise waits for it before showing a prompt.
cat > "$ROOTFS/etc/archrs-init.conf" <<'CONF'
wait /bin/sh /root/setup-testuser.sh
respawn /usr/bin/agetty -L ttyS0 115200 linux
CONF

echo "login-test: building disk image..."
rm -f "$IMAGE" "$SOCK"
truncate -s 1G "$IMAGE"
$FAKEROOT mke2fs -F -t ext4 -d "$ROOTFS" -L archrsroot "$IMAGE" >/dev/null

KVM_ARGS=""
if [ -w /dev/kvm ]; then
    KVM_ARGS="-enable-kvm"
fi

echo "login-test: booting..."
qemu-system-x86_64 \
    -kernel "$KERNEL" \
    -drive file="$IMAGE",format=raw,if=virtio \
    -append "root=/dev/vda rw console=ttyS0 init=/sbin/init.archrs panic=1" \
    -m 1G \
    -serial "unix:$SOCK,server=on,wait=off" \
    -display none \
    -monitor none \
    -no-reboot \
    -net none \
    $KVM_ARGS \
    > "$WORKDIR/login-test-qemu.log" 2>&1 &
QEMU_PID=$!

cleanup() {
    kill "$QEMU_PID" >/dev/null 2>&1 || true
    wait "$QEMU_PID" 2>/dev/null || true
}
trap cleanup EXIT

driver_status=0
python3 "$WORKSPACE_ROOT/scripts/login-test-driver.py" "$SOCK" > "$LOG" 2>&1 || driver_status=$?

echo "login-test: full transcript at $LOG"
echo "---"
cat "$LOG"
echo "---"

if [ "$driver_status" -eq 0 ]; then
    echo "login-test: ALL CHECKS PASSED"
else
    echo "login-test: SOME CHECKS FAILED — see $LOG"
    exit 1
fi
