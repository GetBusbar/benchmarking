#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
import base64
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("ssh_trust", Path(__file__).with_name("ssh_trust.py"))
mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)

class SSHTrustTests(unittest.TestCase):
    def test_only_console_authenticated_key_for_exact_host_is_accepted(self):
        blob = b"test ed25519 public key"
        key = "192.0.2.1 ssh-ed25519 " + base64.b64encode(blob).decode()
        fingerprint = "SHA256:" + base64.b64encode(hashlib.sha256(blob).digest()).decode().rstrip("=")
        self.assertEqual(mod.verified_key("192.0.2.1", key, "256 " + fingerprint + " host (ED25519)"), key)
        for ip, scan, console in [("192.0.2.2", key, fingerprint), ("192.0.2.1", key, "other-key"), ("192.0.2.1", key, fingerprint + "suffix"), ("192.0.2.1", "192.0.2.1 ssh-ed25519 !!!", fingerprint)]:
            with self.subTest(ip=ip, console=console):
                with self.assertRaises(ValueError): mod.verified_key(ip, scan, console)

    def test_update_preserves_other_hosts_and_replaces_only_verified_target(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "known hosts"
            path.write_text("192.0.2.1 ssh-ed25519 old\n192.0.2.2 ssh-ed25519 other\n")
            mod.write_pin(path, "192.0.2.1", "192.0.2.1 ssh-ed25519 new")
            self.assertEqual(path.read_text().splitlines(), ["192.0.2.2 ssh-ed25519 other", "192.0.2.1 ssh-ed25519 new"])
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)

if __name__ == "__main__": unittest.main()
