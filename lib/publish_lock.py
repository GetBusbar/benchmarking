#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Acquire the inherited open-file lock; retain a token directory for old callers."""
import fcntl
import os
from pathlib import Path
import sys
import time
import uuid


def acquire(fd, directory, token, timeout):
    deadline = time.monotonic() + timeout
    while True:
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            pass
        else:
            try:
                directory.mkdir()
            except FileExistsError:
                try:
                    old = (directory / "token").read_text().strip()
                    pid = int(old.rsplit(":", 2)[-2])
                    if pid <= 0:
                        raise ValueError("invalid holder pid")
                    try:
                        os.kill(pid, 0)
                    except ProcessLookupError:
                        # Under the kernel lock only one new caller can reclaim. Old
                        # callers never reclaim: they only remove their own live token.
                        stale = directory.with_name(directory.name + ".stale-" + uuid.uuid4().hex)
                        directory.rename(stale)
                        (stale / "token").unlink()
                        stale.rmdir()
                        fcntl.flock(fd, fcntl.LOCK_UN)
                        continue
                except (OSError, ValueError, IndexError):
                    # Unknown or inaccessible owners are never guessed dead.
                    pass
                fcntl.flock(fd, fcntl.LOCK_UN)
            else:
                try:
                    (directory / "token").write_text(token + "\n")
                except Exception:
                    directory.rmdir()
                    raise
                # The parent still holds this open file description. Exiting this
                # helper leaves its flock in place until the parent closes fd.
                return
        if time.monotonic() >= deadline:
            raise TimeoutError("publisher lock is held (or its legacy owner cannot be verified)")
        time.sleep(0.05)


if __name__ == "__main__":
    acquire(int(sys.argv[1]), Path(sys.argv[2]), sys.argv[3], float(sys.argv[4]))
