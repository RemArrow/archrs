//! `su`/`sudo`/`visudo` — real privilege-management tools, vendoring
//! [`sudo-rs`](https://github.com/trifectatechfoundation/sudo-rs)
//! (Trifecta Tech Foundation's memory-safe reimplementation, already
//! shipping as the default `sudo`/`su` on real distros) rather than
//! hand-rolling privilege-escalation logic — the same "vendor a real
//! crate, stay cautious about reimplementing security-critical code"
//! pattern this project has followed since Phase 1 (real `crypt(3)`
//! for password hashing, real `mount(2)`/`chroot(2)` for privileged
//! syscalls), applied to the single riskiest category of tool in the
//! whole userland.
//!
//! **Deliberately its own crate with three standalone binaries, not
//! part of `coreutils-rs`'s multicall dispatch** — `sudo`/`su` are
//! meaningless without the setuid-root bit, and `sudo-rs` itself
//! refuses to run unless it's genuinely owned by root with that bit
//! set (verified directly: `sudo --version` as a plain file printed
//! "sudo must be owned by uid 0 and have the setuid bit set" and
//! exited 1). Folding that into `coreutils-rs` would mean either
//! making the *entire* ~150-utility multicall binary setuid root
//! (expanding the privilege-escalation attack surface to every `ls`/
//! `cat`/`grep` parsing bug in the binary) or somehow setuid-ing one
//! symlink differently from another sharing the same inode, which
//! isn't how the setuid bit works — it's a property of the file, not
//! the name it's invoked through. Real distros keep `sudo`/`su` as
//! their own small, separately-audited binaries for exactly this
//! reason; this project does the same.
//!
//! Each binary here is a one-line wrapper around `sudo-rs`'s own
//! public `{su,sudo,visudo}_main()` — like `brush_shell::entry::run()`
//! elsewhere in this project, these read real process argv/env
//! directly and call `process::exit` internally, so they need to be
//! genuine binaries with correct argv[0], not something dispatched
//! through an args-iterator abstraction.
//!
//! Deployment (installing setuid-root, real PAM service files, a real
//! `/etc/sudoers`) is exercised in `scripts/boot-test.sh` — see
//! ROADMAP.md's Phase 8 section for what's verified there and what
//! isn't yet.

fn main() {
    sudo_rs::su_main();
}
