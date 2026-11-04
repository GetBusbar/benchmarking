#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Record the source and binary actually built; never infer it from a declared ref."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile


def collect(source, binary):
    def git(*args):
        return subprocess.check_output(["git", "-C", str(source), *args], text=True).strip()
    lock = source / "Cargo.lock"
    if not lock.exists() and (source / "litellm-rust/Cargo.lock").exists():
        lock = source / "litellm-rust/Cargo.lock"
    with binary.open("rb") as artifact:
        binary_digest = hashlib.file_digest(artifact, "sha256").hexdigest()
    return {
        "source_commit": git("rev-parse", "HEAD"),
        "source_dirty": bool(git("status", "--porcelain", "--untracked-files=no")),
        "binary_name": binary.name,
        "binary_sha256": binary_digest,
        "cargo_lock_sha256": hashlib.sha256(lock.read_bytes()).hexdigest() if lock.exists() else None,
        "compiler": subprocess.check_output(["rustc", "-Vv"], text=True).strip(),
    }


def write(source, binary, destination):
    proof = collect(source, binary)
    fd, tmp = tempfile.mkstemp(prefix=destination.name + ".", dir=destination.parent)
    try:
        with os.fdopen(fd, "w") as out:
            json.dump(proof, out, indent=2); out.write("\n")
            out.flush(); os.fsync(out.fileno())
        os.replace(tmp, destination)
    finally:
        if os.path.exists(tmp): os.unlink(tmp)


if __name__ == "__main__":
    write(Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3]))
