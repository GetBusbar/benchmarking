#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("gateway_proof", Path(__file__).with_name("gateway_build_provenance.py"))
mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)

class BuiltGatewayTests(unittest.TestCase):
    def test_receipt_contains_actual_binary_lock_and_dirty_source(self):
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp); binary = src / "gateway"; binary.write_bytes(b"compiled bytes")
            (src / "Cargo.lock").write_bytes(b"locked crates")
            dest = src / "receipt.json"
            def output(args, **kwargs):
                if args[0] == "rustc": return "rustc test compiler"
                if "rev-parse" in args: return "a" * 40
                return " M Cargo.lock"
            with patch.object(mod.subprocess, "check_output", side_effect=output):
                mod.write(src, binary, dest)
            proof = json.loads(dest.read_text())
            self.assertEqual(proof["source_commit"], "a" * 40)
            self.assertTrue(proof["source_dirty"])
            self.assertEqual(proof["binary_sha256"], hashlib.sha256(binary.read_bytes()).hexdigest())
            self.assertEqual(proof["cargo_lock_sha256"], hashlib.sha256((src / "Cargo.lock").read_bytes()).hexdigest())
            self.assertEqual(proof["compiler"], "rustc test compiler")

if __name__ == "__main__": unittest.main()
