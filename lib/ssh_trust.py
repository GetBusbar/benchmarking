#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Pin an EC2 SSH key only after AWS console output authenticates its fingerprint."""
import base64
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time


def verified_key(ip, scan, console):
    ipaddress.ip_address(ip)
    for line in scan.splitlines():
        fields = line.split()
        if len(fields) != 3 or fields[:2] != [ip, "ssh-ed25519"]:
            continue
        try:
            blob = base64.b64decode(fields[2], validate=True)
        except ValueError:
            continue
        fingerprint = "SHA256:" + base64.b64encode(hashlib.sha256(blob).digest()).decode().rstrip("=")
        if fingerprint in console.split():
            return line
    raise ValueError("SSH key is not authenticated by this EC2 instance's console fingerprint")


def write_pin(path, ip, line):
    with open(str(path) + ".lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        old = path.read_text().splitlines() if path.exists() else []
        lines = [s for s in old if s.split() and s.split()[0] != ip] + [line]
        fd, tmp = tempfile.mkstemp(prefix=path.name + ".", dir=path.parent)
        try:
            with os.fdopen(fd, "w") as out:
                out.write("\n".join(lines) + "\n")
                out.flush(); os.fsync(out.fileno())
            os.replace(tmp, path)
        finally:
            if os.path.exists(tmp): os.unlink(tmp)


def pin(instance, ip, path):
    if not re.fullmatch(r"i-[0-9a-f]+", instance):
        raise ValueError("invalid instance identity")
    ipaddress.ip_address(ip)
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        try:
            response = subprocess.run(["bench-aws", "ec2", "get-console-output", "--instance-id", instance, "--latest", "--output", "json"], capture_output=True, text=True, timeout=15, check=True)
            console = json.loads(response.stdout).get("Output", "")
            scan = subprocess.run(["ssh-keyscan", "-T", "3", "-t", "ed25519", ip], capture_output=True, text=True, timeout=6, check=True)
            line = verified_key(ip, scan.stdout, console)
            write_pin(path, ip, line)
            print(f"authenticated EC2 SSH host key for {instance} at {ip}")
            return
        except (ValueError, OSError, subprocess.SubprocessError):
            time.sleep(2)
    raise TimeoutError("AWS console could not authenticate the SSH key within 120 seconds; refusing connection")


if __name__ == "__main__":
    pin(sys.argv[1], sys.argv[2], Path(sys.argv[3]))
