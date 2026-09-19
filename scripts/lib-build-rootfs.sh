# Shared by scripts/boot-test.sh, scripts/login-test.sh, and
# scripts/network-test.sh: populates $ROOTFS with a real base system plus
# every archrs binary, so the three scripts can't drift apart on how the
# image is actually built — only on what systemd units they enable and
# how they drive the boot. Real systemd is this project's own real init
# as of Phase 13 (replaced its own former archrs-init — see ROADMAP.md),
# so "how a test script runs automatically at boot" is now "write and
# enable a real systemd unit" (see write_boot_service below) rather than
# writing a line into archrs-init.conf's own respawn/wait list.
#
# Meant to be sourced (`. "$(dirname "$0")/lib-build-rootfs.sh"`), not
# run directly. Expects WORKSPACE_ROOT, TARGET, WORKDIR, ROOTFS, and
# FAKEROOT (may be empty) already set by the caller; sets KERNEL if the
# caller hasn't already exported ARCHRS_BOOT_TEST_KERNEL.
#
# write_boot_service UNIT_NAME EXEC_START — writes a systemd oneshot
# service running EXEC_START and enables it via multi-user.target, using
# real systemctl's own real offline support for exactly this
# ("systemctl --root=DIR enable UNIT ... Edit/enable/disable/mask unit
# files in the specified root directory" — same technique
# crates/archrs-install/src/build.rs uses). Runs on the *host's* own
# real systemctl (already installed on this dev machine), not
# anything from inside $ROOTFS — no chroot needed, matching every other
# privileged-tool invocation in this project's scripts.
write_boot_service() {
    unit_name="$1"
    exec_start="$2"
    mkdir -p "$ROOTFS/etc/systemd/system"
    cat > "$ROOTFS/etc/systemd/system/$unit_name" <<SERVICE
[Unit]
Description=archrs test script

[Service]
Type=oneshot
ExecStart=$exec_start
StandardOutput=journal+console
StandardError=journal+console

[Install]
WantedBy=multi-user.target
SERVICE
    systemctl --root="$ROOTFS" enable "$unit_name"
}

if [ -z "$KERNEL" ]; then
    KERNEL=$(ls /boot/vmlinuz-* 2>/dev/null | head -1)
fi
if [ -z "$KERNEL" ] || [ ! -r "$KERNEL" ]; then
    echo "$0: no readable kernel image found (set ARCHRS_BOOT_TEST_KERNEL)" >&2
    exit 1
fi

for bin in coreutils-rs pacman-rs crond crontab su sudo visudo; do
    if [ ! -x "$TARGET/$bin" ]; then
        echo "$0: $TARGET/$bin missing — run 'cargo build --release' first" >&2
        exit 1
    fi
done

mkdir -p "$ROOTFS"

if [ ! -f "$ROOTFS/.base-installed" ]; then
    # pam/sudo/shadow/util-linux pulled in purely for what this project
    # doesn't reimplement: libpam.so + its real pam_unix/pam_rootok/...
    # modules, real /etc/pam.d/{su,sudo,login,system-auth}, /etc/sudoers,
    # /etc/login.defs config files, and (util-linux specifically) real
    # `agetty`/`login`. `systemd`/`dbus` are this project's real init as
    # of Phase 13 (replaced this project's own former `archrs-init` —
    # see ROADMAP.md). Every binary any of these packages ship gets
    # immediately overwritten below by this project's own
    # coreutils-rs/privtools-rs equivalents where one exists — only the
    # supporting files are actually used from them, same pattern as
    # glibc/filesystem/bash/xz/file.
    echo "$0: installing base system into $ROOTFS (glibc, filesystem, bash, xz, file, pam, sudo, shadow, util-linux, kmod, systemd, dbus)..."
    $FAKEROOT "$TARGET/pacman-rs" -Sy --root "$ROOTFS"
    $FAKEROOT "$TARGET/pacman-rs" -S glibc filesystem bash xz file pam sudo shadow util-linux kmod systemd dbus --root "$ROOTFS"
    touch "$ROOTFS/.base-installed"
fi

# `systemd-firstboot.service` is a real, "static" unit (confirmed via
# `systemctl list-unit-files` on this dev machine) that runs
# unconditionally on first real boot and, on a genuinely unconfigured
# system, interactively prompts on the console for a timezone/locale —
# confirmed the hard way: the first version of this test hung at
# "Please enter the new timezone name..." until the outer `timeout`
# killed it. Pre-populating what it would have asked for, then masking
# it (the correct way to disable a "static" unit — it has no
# `[Install]` section to plainly disable), avoids that entirely. A real
# `archrs-install` deployment needs the identical fix (see its own
# `build.rs`) — this isn't test-script-specific.
mkdir -p "$ROOTFS/etc"
ln -sf /usr/share/zoneinfo/UTC "$ROOTFS/etc/localtime"
echo "LANG=C.UTF-8" > "$ROOTFS/etc/locale.conf"
systemctl --root="$ROOTFS" mask systemd-firstboot.service

# $ROOTFS (and its downloaded packages) is deliberately shared and
# cached across all three test scripts — but unlike the old
# archrs-init.conf (a single file each script simply overwrote), real
# systemd units *persist*: enabling one doesn't remove any other
# script's earlier one. Confirmed for real as a genuine bug, not a
# hypothetical: running login-test.sh after boot-test.sh had already
# run against the same $ROOTFS caused *both* scripts' services to
# start during the same boot, and boot-test.sh's own script calling
# `poweroff` partway through shut the VM down before login-test.sh's
# driver ever got to interact with it. Each script now clears every
# known test unit before enabling its own.
for unit in archrs-boot-test.service archrs-network-test.service archrs-setup-testuser.service; do
    systemctl --root="$ROOTFS" disable "$unit" >/dev/null 2>&1 || true
    rm -f "$ROOTFS/etc/systemd/system/$unit"
done
rm -rf "$ROOTFS/etc/systemd/system/serial-getty@ttyS0.service.d"
systemctl --root="$ROOTFS" disable serial-getty@ttyS0.service >/dev/null 2>&1 || true

echo "$0: installing archrs binaries..."
mkdir -p "$ROOTFS/usr/local/bin"
for bin in coreutils-rs pacman-rs makepkg-rs crond crontab; do
    $FAKEROOT cp "$TARGET/$bin" "$ROOTFS/usr/local/bin/$bin"
done

# su/sudo/visudo: real standalone setuid-root binaries in real distros —
# deliberately NOT symlinked through coreutils-rs's multicall dispatch
# (see crates/privtools-rs's doc comment). Installed straight to /usr/bin,
# overwriting the real util-linux/sudo package's own binaries but keeping
# their PAM/sudoers config files.
#
# `chmod u+s` is gated on `$FAKEROOT` being active, not unconditional —
# found the hard way (a real regression, caught by testing the no-fakeroot
# fallback path directly): on `execve`, the kernel honors a setuid file's
# own *real, on-disk* owner regardless of who invoked it, even root — so
# a setuid bit on a binary that's genuinely still host-uid-owned (no
# fakeroot) doesn't just fail to help, it actively downgrades a real-root
# invoker's effective uid to the host's own uid on exec, breaking `su`
# outright. Under `$FAKEROOT`, both the root ownership and this setuid
# bit are faked consistently and really do land in the final ext4 image
# (verified with `debugfs -R stat`), so the bit is only ever set together
# with genuine (faked) root ownership.
for bin in su sudo visudo; do
    $FAKEROOT cp "$TARGET/$bin" "$ROOTFS/usr/bin/$bin"
    if [ -n "$FAKEROOT" ]; then
        $FAKEROOT chmod u+s "$ROOTFS/usr/bin/$bin"
    fi
done

# Symlink every utility coreutils-rs dispatches, straight from its own
# source of truth, so this list can never drift out of sync with it. One
# single $FAKEROOT invocation wrapping the whole loop, not one per
# symlink — fakeroot's per-process startup/state-reload cost would
# otherwise be paid ~170 times over. A real temp script file, not an
# inline `sh -c "..."` string, to sidestep having to reason about two
# nested layers of shell-quoting for the same `$util` variable.
grep -oP '^\s*\("\K[^"]+' "$WORKSPACE_ROOT/crates/coreutils-rs/src/util_list.rs" | sort -u > "$WORKDIR/util-names.txt"
cat > "$WORKDIR/symlink-utils.sh" <<EOF
#!/bin/sh
while IFS= read -r util; do
    ln -sf /usr/local/bin/coreutils-rs "$ROOTFS/usr/bin/\$util"
done < "$WORKDIR/util-names.txt"
EOF
$FAKEROOT sh "$WORKDIR/symlink-utils.sh"
