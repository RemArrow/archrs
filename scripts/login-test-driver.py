#!/usr/bin/env python3
"""Drives a real interactive login session over a QEMU serial unix socket.

Minimal expect(1)-alike (connect/send/expect-with-timeout) built on
`socket`+`select` rather than depending on the `expect` package, which
isn't installed on the dev machine this was written on. Everything read
from the socket is echoed to stdout as it arrives, so the caller's saved
log has the real session transcript, not just this script's own verdicts.

Usage: login-test-driver.py <unix-socket-path>
Exit code 0 only if every step below actually observed the real text it
expected; prints one "LOGIN-TEST: ..." line per step either way.
"""

import select
import socket
import sys
import time


class ExpectTimeout(Exception):
    pass


def connect(path, timeout=30):
    deadline = time.time() + timeout
    last_err = None
    while time.time() < deadline:
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.connect(path)
            return s
        except (FileNotFoundError, ConnectionRefusedError) as e:
            last_err = e
            time.sleep(0.2)
    raise ExpectTimeout(f"could not connect to {path}: {last_err}")


DSR_CURSOR_QUERY = b"\x1b[6n"
DSR_CURSOR_REPLY = b"\x1b[24;1R"


def expect(sock, patterns, timeout=45):
    if isinstance(patterns, (str, bytes)):
        patterns = [patterns]
    patterns = [p.encode() if isinstance(p, str) else p for p in patterns]
    buf = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        r, _, _ = select.select([sock], [], [], 1.0)
        if r:
            chunk = sock.recv(4096)
            if not chunk:
                break
            buf += chunk
            sys.stdout.write(chunk.decode(errors="replace"))
            sys.stdout.flush()
            # Real interactive shells (this project's own `brush`
            # included) probe cursor position via this ANSI query when
            # they can't otherwise size the terminal — any real terminal
            # program (minicom, screen, a physical console's own VT)
            # answers it automatically; this raw socket has to do the
            # same or the shell blocks waiting for a reply that never
            # comes (confirmed for real: brush printed "input error
            # occurred: The cursor position could not be read" and its
            # session died before this was added).
            if DSR_CURSOR_QUERY in chunk:
                sock.sendall(DSR_CURSOR_REPLY)
        for i, p in enumerate(patterns):
            if p in buf:
                # Small settle delay: the matched substring can arrive
                # slightly before the rest of its own line/prompt (seen
                # for real with "incorrect" landing before the fresh
                # "login:" that follows it) — give trailing output a
                # moment to land before the caller's next send() risks
                # racing it.
                time.sleep(0.3)
                return i, buf
    raise ExpectTimeout(
        f"timed out after {timeout}s waiting for {patterns!r}; "
        f"buffer so far: {buf.decode(errors='replace')!r}"
    )


def send(sock, text):
    sock.sendall(text.encode())


def drain(sock, duration=1.5):
    """Reads (and answers any DSR cursor query in) whatever arrives for
    `duration` seconds, without requiring a match — used for windows
    between sends where nothing specific needs waiting for, but a DSR
    query might still land and needs a live reader to answer it (see
    `expect`'s own comment on why)."""
    deadline = time.time() + duration
    while time.time() < deadline:
        r, _, _ = select.select([sock], [], [], 0.2)
        if r:
            chunk = sock.recv(4096)
            if not chunk:
                return
            sys.stdout.write(chunk.decode(errors="replace"))
            sys.stdout.flush()
            if DSR_CURSOR_QUERY in chunk:
                sock.sendall(DSR_CURSOR_REPLY)


def main():
    if len(sys.argv) != 2:
        print("usage: login-test-driver.py <unix-socket-path>", file=sys.stderr)
        return 2
    sock_path = sys.argv[1]

    results = []

    def step(name, fn):
        try:
            fn()
            print(f"LOGIN-TEST: {name}: PASS")
            results.append(True)
        except ExpectTimeout as e:
            print(f"LOGIN-TEST: {name}: FAIL ({e})")
            results.append(False)

    s = connect(sock_path)

    def do_boot_prompt():
        expect(s, "login:", timeout=60)

    step("real getty login prompt appeared", do_boot_prompt)
    if not results[-1]:
        return 1

    def do_bad_password():
        send(s, "testuser\r")
        expect(s, "Password:", timeout=15)
        send(s, "wrongpassword\r")
        idx, buf = expect(s, ["incorrect", "Incorrect"], timeout=15)
        # login(1) re-prompts "login:" itself after a failure (a few
        # retries within the same process, or a fresh agetty respawn —
        # either way a real "login:" reappears on its own timing, not
        # necessarily already in the buffer the moment "incorrect"
        # shows up). Wait for it for real before sending anything else —
        # sending the next username too early raced the actual prompt
        # here the first time this was tested, desyncing every step
        # after it (each input landing one prompt too early).
        if b"login:" not in buf:
            expect(s, "login:", timeout=15)

    step("wrong password rejected by real PAM auth", do_bad_password)

    def do_good_login():
        send(s, "testuser\r")
        expect(s, "Password:", timeout=15)
        send(s, "secret123\r")
        drain(s, 1.5)
        send(s, "id -un\r")
        expect(s, "testuser", timeout=20)

    step("real login succeeded (crypt/shadow auth, uid switch, shell exec)", do_good_login)

    def do_unprivileged_poweroff():
        send(s, "poweroff\r")
        expect(s, ["Operation not permitted", "could not signal"], timeout=15)

    step("unprivileged poweroff correctly refused", do_unprivileged_poweroff)

    def do_logout_respawn():
        send(s, "exit\r")
        expect(s, "login:", timeout=20)

    step("agetty respawned a fresh login prompt after logout", do_logout_respawn)

    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
