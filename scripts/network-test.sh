#!/bin/sh
# Boots the same real archrs image as scripts/boot-test.sh, but with a
# real NIC attached (QEMU's usermode/"slirp" networking — no host root
# or bridge/tap setup needed, just like everything else this project's
# test scripts do) and checks that this project's own `dhcpc` can
# actually get the VM onto a network: bring the interface up, run a
# real DHCP exchange against slirp's own built-in DHCP server, get a
# real lease, configure the address/default route/resolv.conf from it,
# and then actually reach the open internet through slirp's NAT (not
# just the gateway) — the point of all of Phase 9's networking work.
#
# Usage: scripts/network-test.sh [--clean]

set -eu

WORKSPACE_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="$WORKSPACE_ROOT/target/release"
WORKDIR="${ARCHRS_BOOT_TEST_DIR:-/tmp/archrs-boot-test}"
ROOTFS="$WORKDIR/rootfs"
IMAGE="$WORKDIR/rootfs-network.img"
LOG="$WORKDIR/network-test.log"
KERNEL="${ARCHRS_BOOT_TEST_KERNEL:-}"
FAKEROOT_STATE="$WORKDIR/fakeroot.state"

if command -v fakeroot >/dev/null 2>&1; then
    FAKEROOT="fakeroot -i $FAKEROOT_STATE -s $FAKEROOT_STATE --"
else
    FAKEROOT=""
fi

if [ "${1:-}" = "--clean" ]; then
    rm -rf "$WORKDIR"
fi

. "$WORKSPACE_ROOT/scripts/lib-build-rootfs.sh"

# The NIC needs a real kernel module (`virtio_net`, plus its own real
# dependency chain `failover` -> `net_failover`) to show up at all —
# confirmed for real by *not* having this first: `ip addr` inside the
# VM showed only `lo`, nothing else, with virtio-net-pci attached but
# invisible to the kernel. This project's boot tests always boot the
# *host's own* kernel image (`ls /boot/vmlinuz-*`, see boot-test.sh),
# so the host's own already-installed `/lib/modules/$(uname -r)/` is,
# by construction, an exact version match — no separate kernel-modules
# package needs downloading just for this test. A real distributable
# install would get its modules the normal way, by installing a real
# `linux` package via pacman-rs alongside its matching kernel, which
# this test's own minimal image deliberately doesn't do (see
# ROADMAP.md's own "what functional doesn't mean" list). Copying just
# the three actually-needed real `.ko.zst` files (not the whole ~170MB
# tree) and loading them in real dependency order with this project's
# own `insmod` doubles as the first genuine insert/remove-cycle
# verification for kmod_cmd.rs, closing a gap that phase's own ROADMAP
# entry explicitly named as not yet reachable.
KVER="$(uname -r)"
MOD_DEST="$ROOTFS/lib/modules/$KVER/extra"
mkdir -p "$MOD_DEST"
for m in \
    "/lib/modules/$KVER/kernel/net/core/failover.ko.zst" \
    "/lib/modules/$KVER/kernel/drivers/net/net_failover.ko.zst" \
    "/lib/modules/$KVER/kernel/drivers/net/virtio_net.ko.zst"
do
    if [ -f "$m" ]; then
        cp "$m" "$MOD_DEST/"
    else
        echo "network-test: warning: $m not found on host — network test will likely fail" >&2
    fi
done

cat > "$ROOTFS/root/network-test.sh" <<'SCRIPT'
#!/usr/bin/sh
echo "NETWORK-TEST: starting"
echo "NETWORK-TEST: loading virtio_net module chain:"
# The guest boots this exact same host's own kernel image (see this
# script's own top-of-file comment), so `uname -r` in here already
# names the right module directory — no need to bake the version in
# from outside and juggle heredoc quoting for it.
KMODDIR="/lib/modules/$(uname -r)/extra"
insmod "$KMODDIR/failover.ko.zst"
insmod "$KMODDIR/net_failover.ko.zst"
insmod "$KMODDIR/virtio_net.ko.zst"
echo "NETWORK-TEST: lsmod after insmod: $(lsmod | tr '\n' '|')"
echo "NETWORK-TEST: dhcpc:"
dhcpc eth0
echo "NETWORK-TEST: ping_group_range before: $(cat /proc/sys/net/ipv4/ping_group_range)"
# ping_cmd.rs's own unprivileged ICMP DGRAM socket needs this sysctl to
# actually allow it — confirmed for real to default to "1 0" (an empty,
# inverted range that permits no group at all) on a bare kernel with
# nothing setting it. Real distros ship a default via a sysctl.d file
# (Arch's own default sets exactly this range) that this minimal image
# doesn't have anything providing yet — written directly here rather
# than adding a `sysctl` command this project doesn't have either.
echo "0 2147483647" > /proc/sys/net/ipv4/ping_group_range
echo "NETWORK-TEST: ip addr after dhcpc:"
ip addr | tr '\n' '|'
echo
echo "NETWORK-TEST: ip route after dhcpc:"
ip route | tr '\n' '|'
echo
echo "NETWORK-TEST: resolv.conf: $(cat /etc/resolv.conf 2>&1 | tr '\n' '|')"
echo "NETWORK-TEST: ping gateway: $(ping -c 2 10.0.2.2 >/dev/null 2>&1; echo $?)"
echo "NETWORK-TEST: ping real internet (1.1.1.1): $(ping -c 2 1.1.1.1 >/dev/null 2>&1; echo $?)"
echo "NETWORK-TEST: curl real internet exit code: $(curl -s -o /dev/null http://example.com >/dev/null 2>&1; echo $?)"
echo "NETWORK-TEST: all checks complete, powering off"
poweroff
sleep 5
echo "NETWORK-TEST: FAIL: still alive after poweroff"
SCRIPT
chmod +x "$ROOTFS/root/network-test.sh"
echo "/bin/sh /root/network-test.sh" > "$ROOTFS/etc/archrs-init.conf"

echo "network-test: building disk image..."
rm -f "$IMAGE"
truncate -s 1G "$IMAGE"
$FAKEROOT mke2fs -F -t ext4 -d "$ROOTFS" -L archrsroot "$IMAGE" >/dev/null

KVM_ARGS=""
if [ -w /dev/kvm ]; then
    KVM_ARGS="-enable-kvm"
fi

echo "network-test: booting..."
timeout 60 qemu-system-x86_64 \
    -kernel "$KERNEL" \
    -drive file="$IMAGE",format=raw,if=virtio \
    -append "root=/dev/vda rw console=ttyS0 init=/sbin/init.archrs panic=1" \
    -m 1G \
    -nographic \
    -no-reboot \
    -netdev user,id=n0 \
    -device virtio-net-pci,netdev=n0 \
    $KVM_ARGS \
    > "$LOG" 2>&1 || true

echo "network-test: full log at $LOG"
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

check "NETWORK-TEST: lsmod after insmod:"
check "dhcpc: sending DISCOVER on eth0"
check "dhcpc: got OFFER of"
check "dhcpc: sending REQUEST for"
check "dhcpc: eth0 configured:"
check "NETWORK-TEST: ping gateway: 0"
check "NETWORK-TEST: ping real internet (1.1.1.1): 0"
check "NETWORK-TEST: curl real internet exit code: 0"
check "archrs-init: powering off"
check "reboot: Power down"

if [ "$fail" -eq 0 ]; then
    echo "---"
    echo "network-test: ALL CHECKS PASSED"
else
    echo "---"
    echo "network-test: SOME CHECKS FAILED — see $LOG"
    exit 1
fi
