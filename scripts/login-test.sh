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

cat > "$ROOTFS/root/setup-testuser.sh" <<'SETUP'
#!/bin/sh
useradd -m -s /bin/sh testuser 2>/dev/null
echo testuser:secret123 | chpasswd
SETUP
chmod +x "$ROOTFS/root/setup-testuser.sh"

# Real systemd is this project's real init as of Phase 13 (replaced its
# own former archrs-init — see ROADMAP.md), so "run this once before the
# login prompt appears" is now a real systemd unit with a real ordering
# dependency, not a line in archrs-init.conf's own respawn/wait list.
# `Before=` on both the getty target and the specific unit (not just
# `Before=getty.target`) because `serial-getty@ttyS0.service` is only
# pulled in by `getty.target` as an already-enabled instance, not
# spawned fresh by it — an ordering edge against the target alone
# doesn't guarantee this runs first, found by reading systemd's own
# `getty.target`/`getty@.service` unit files on this dev machine rather
# than guessing.
mkdir -p "$ROOTFS/etc/systemd/system"
cat > "$ROOTFS/etc/systemd/system/archrs-setup-testuser.service" <<'UNIT'
[Unit]
Description=archrs login-test setup
Before=getty.target serial-getty@ttyS0.service

[Service]
Type=oneshot
ExecStart=/root/setup-testuser.sh
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
UNIT
systemctl --root="$ROOTFS" enable archrs-setup-testuser.service
# `-L` (ignore modem control lines): QEMU's virtual serial port never
# raises DCD, and agetty otherwise waits for it before showing a prompt.
# Real `serial-getty@.service` doesn't pass `-L` by default, so a
# drop-in overrides its `ExecStart` rather than using the plain
# `systemctl enable serial-getty@ttyS0.service` this project's own
# `archrs-install` uses for real hardware (which has real modem control
# lines, or none needed for a local `tty1` — this is specifically a
# QEMU-serial-console workaround, not something a real install needs).
mkdir -p "$ROOTFS/etc/systemd/system/serial-getty@ttyS0.service.d"
cat > "$ROOTFS/etc/systemd/system/serial-getty@ttyS0.service.d/override.conf" <<'UNIT'
[Service]
ExecStart=
ExecStart=-/usr/bin/agetty -L %I 115200 linux
UNIT
systemctl --root="$ROOTFS" enable serial-getty@ttyS0.service

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
    -append "root=/dev/vda rw console=ttyS0 panic=1 init=/usr/lib/systemd/systemd" \
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
