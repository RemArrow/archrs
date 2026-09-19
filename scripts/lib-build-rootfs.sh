# Shared by scripts/boot-test.sh and scripts/login-test.sh: populates
# $ROOTFS with a real base system plus every archrs binary, so the two
# scripts can't drift apart on how the image is actually built — only on
# what they put in /etc/archrs-init.conf and how they drive the boot.
#
# Meant to be sourced (`. "$(dirname "$0")/lib-build-rootfs.sh"`), not
# run directly. Expects WORKSPACE_ROOT, TARGET, WORKDIR, ROOTFS, and
# FAKEROOT (may be empty) already set by the caller; sets KERNEL if the
# caller hasn't already exported ARCHRS_BOOT_TEST_KERNEL.

if [ -z "$KERNEL" ]; then
    KERNEL=$(ls /boot/vmlinuz-* 2>/dev/null | head -1)
fi
if [ -z "$KERNEL" ] || [ ! -r "$KERNEL" ]; then
    echo "$0: no readable kernel image found (set ARCHRS_BOOT_TEST_KERNEL)" >&2
    exit 1
fi

for bin in coreutils-rs pacman-rs archrs-init crond crontab su sudo visudo; do
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
    # `agetty`/`login`. Every binary any of these packages ship gets
    # immediately overwritten below by this project's own
    # coreutils-rs/privtools-rs equivalents where one exists — only the
    # supporting files are actually used from them, same pattern as
    # glibc/filesystem/bash/xz/file.
    echo "$0: installing base system into $ROOTFS (glibc, filesystem, bash, xz, file, pam, sudo, shadow, util-linux, kmod)..."
    $FAKEROOT "$TARGET/pacman-rs" -Sy --root "$ROOTFS"
    $FAKEROOT "$TARGET/pacman-rs" -S glibc filesystem bash xz file pam sudo shadow util-linux kmod --root "$ROOTFS"
    touch "$ROOTFS/.base-installed"
fi

echo "$0: installing archrs binaries..."
mkdir -p "$ROOTFS/usr/local/bin"
for bin in coreutils-rs pacman-rs makepkg-rs archrs-init crond crontab; do
    $FAKEROOT cp "$TARGET/$bin" "$ROOTFS/usr/local/bin/$bin"
done
rm -f "$ROOTFS/sbin/init.archrs"
$FAKEROOT cp "$TARGET/archrs-init" "$ROOTFS/sbin/init.archrs"

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
