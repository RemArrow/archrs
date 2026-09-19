#!/usr/bin/env python3
"""Drives the real text editor (`coreutils-rs nano`, kiro_cmd.rs) under
a real pseudo-terminal and checks it genuinely edits and saves a file.

A bare `pty.fork()` child starts with a 0x0 window size until something
sets one — real terminal emulators always have a real size already;
this script sets one explicitly via `TIOCSWINSZ` before exec, matching
what a real terminal does, rather than needing to also answer
kiro-editor's ANSI cursor-position fallback query (confirmed for real,
the first time this was run without doing either: the editor just hung
waiting for a reply nothing was giving it — the same class of issue
already found and fixed for `brush` in login-test-driver.py).

Usage: editor-test-driver.py <path-to-coreutils-rs>
"""

import fcntl
import os
import pty
import signal
import struct
import sys
import select
import termios
import time

RECV_TIMEOUT = 1.0


def drain(fd, duration):
    end = time.time() + duration
    buf = b""
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            try:
                chunk = os.read(fd, 4096)
            except OSError:
                break
            if not chunk:
                break
            buf += chunk
    return buf


def main():
    if len(sys.argv) != 2:
        print("usage: editor-test-driver.py <path-to-coreutils-rs>", file=sys.stderr)
        return 2
    coreutils_rs = sys.argv[1]

    testfile = "/tmp/archrs-editor-test-file.txt"
    with open(testfile, "w") as f:
        f.write("line one\n")

    results = []

    def check(name, cond):
        print(f"EDITOR-TEST: {name}: {'PASS' if cond else 'FAIL'}")
        results.append(cond)

    pid, fd = pty.fork()
    if pid == 0:
        os.execv(coreutils_rs, [coreutils_rs, "nano", testfile])

    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))

    time.sleep(0.5)
    screen = drain(fd, 1.0)
    check(
        "editor rendered a real screen with the file's content",
        b"line one" in screen and b"Ctrl-? for help" in screen,
    )

    os.write(fd, b" appended-by-test")
    drain(fd, 0.5)

    os.write(fd, b"\x13")  # Ctrl-S: save
    drain(fd, 1.0)
    with open(testfile) as f:
        after_save = f.read()
    check(
        "Ctrl-S actually saved the real file while the editor was still running",
        after_save == " appended-by-testline one\n",
    )

    os.write(fd, b"\x11")  # Ctrl-Q: quit
    deadline = time.time() + 3
    exited = False
    while time.time() < deadline:
        wpid, _ = os.waitpid(pid, os.WNOHANG)
        if wpid != 0:
            exited = True
            break
        time.sleep(0.1)
    if not exited:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
    check("Ctrl-Q quit the editor process cleanly (no force-kill needed)", exited)

    os.close(fd)
    os.remove(testfile)

    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
