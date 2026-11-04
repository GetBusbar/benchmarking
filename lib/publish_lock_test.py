#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HELPER = Path(__file__).with_name("publish_lock.py")
spec = importlib.util.spec_from_file_location("publish_lock", HELPER)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

class PublishLockTests(unittest.TestCase):
    def test_dead_legacy_holder_is_reclaimed(self):
        dead = subprocess.Popen([sys.executable, "-c", "pass"])
        dead.wait()
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp) / "lock.d"; d.mkdir()
            (d / "token").write_text(f"old:tag:{dead.pid}:nonce")
            with open(Path(tmp) / "lock", "a") as f:
                mod.acquire(f.fileno(), d, f"new:tag:{os.getpid()}:nonce", 0.2)
                self.assertEqual((d / "token").read_text().strip(), f"new:tag:{os.getpid()}:nonce")

    def test_live_legacy_holder_is_never_removed(self):
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp) / "lock.d"; d.mkdir()
            token = f"old:tag:{os.getpid()}:nonce"
            (d / "token").write_text(token)
            with open(Path(tmp) / "lock", "a") as f:
                with self.assertRaises(TimeoutError): mod.acquire(f.fileno(), d, "new:tag:1:nonce", 0.1)
            self.assertEqual((d / "token").read_text(), token)

    def test_helper_exit_keeps_kernel_lock_until_parent_closes_descriptor(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "lock"; d = Path(tmp) / "lock.d"
            with open(path, "a") as f:
                token = f"run:tag:{os.getpid()}:nonce"
                subprocess.run([sys.executable, str(HELPER), str(f.fileno()), str(d), token, "0.1"], pass_fds=(f.fileno(),), check=True)
                # Remove the compatibility token to prove contention comes from the
                # inherited kernel lock, not merely from the legacy directory.
                (d / "token").unlink(); d.rmdir()
                with open(path, "a") as contender:
                    with self.assertRaises(TimeoutError): mod.acquire(contender.fileno(), d, "other:tag:1:nonce", 0.1)
            with open(path, "a") as contender:
                mod.acquire(contender.fileno(), d, f"new:tag:{os.getpid()}:nonce", 0.1)

if __name__ == "__main__": unittest.main()
