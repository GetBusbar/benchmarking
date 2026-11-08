#!/usr/bin/env python3
"""Reject mismatched or incomplete instrument bundles before paying for EC2."""
import hashlib
from pathlib import Path
import sys


def verify(directory, commit, arch):
    root = Path(directory)
    if (root / 'engine-commit').read_text().strip() != commit:
        raise ValueError('Artifact commit differs from ENGINE_PIN')
    expected = {f'otb-{arch}', f'mock-{arch}'}
    seen = set()
    for line in (root / 'SHA256SUMS').read_text().splitlines():
        digest, name = line.split()
        name = Path(name).name
        if name not in expected or name in seen:
            raise ValueError('unexpected or duplicate rig file')
        data = (root / name).read_bytes()
        if hashlib.sha256(data).hexdigest() != digest:
            raise ValueError(f'checksum mismatch: {name}')
        machine = {'arm64': 183, 'x86_64': 62, 'x86': 62}[arch]
        if data[:6] != b'\x7fELF\x02\x01' or int.from_bytes(data[18:20], 'little') != machine:
            raise ValueError(f'wrong executable architecture: {name}')
        seen.add(name)
    if seen != expected:
        raise ValueError('incomplete rig bundle')


if __name__ == '__main__':
    verify(*sys.argv[1:])
