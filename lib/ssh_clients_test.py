#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Exercise EC2 client trust ordering with mocked AWS, SSH and rsync only."""
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]
CLIENTS = ["run-mutants-ec2.sh", "watch-mutants.sh", "watch-busbar.sh",
           "watch-orphans.sh", "bench-dashboard.py"]
IP = "192.0.2.1"
INSTANCE = "i-0123456789abcdef0"
KEY = base64.b64encode(b"fixture public host key").decode()
FINGERPRINT = "SHA256:" + base64.b64encode(
    hashlib.sha256(base64.b64decode(KEY)).digest()).decode().rstrip("=")


class SSHClientTests(unittest.TestCase):
    def run_client(self, client, refuse=False, ambiguous=False):
        with tempfile.TemporaryDirectory(prefix="ssh clients ") as tmp:
            root = Path(tmp)
            source, bins, state = root / "repo", root / "bin", root / "state with spaces"
            source.mkdir(); bins.mkdir(); state.mkdir()
            (source / "lib").mkdir()
            shutil.copy2(REPO / client, source / client)
            shutil.copy2(REPO / "lib/ssh_trust.py", source / "lib/ssh_trust.py")
            (source / "results/snapshots").mkdir(parents=True)
            (state / "gateway-bench-key.pem").write_text("fixture, not a private key\n")
            calls = root / "calls.jsonl"
            environment = dict(os.environ, PATH=str(bins) + os.pathsep + os.environ["PATH"],
                BENCH_STATE_DIR=str(state), CALLS=str(calls), FIXTURE_IP=IP,
                FIXTURE_INSTANCE=INSTANCE, FIXTURE_KEY=KEY,
                FIXTURE_FINGERPRINT=FINGERPRINT, AUTH_REFUSE=str(int(refuse)),
                AMBIGUOUS=str(int(ambiguous)), CLIENT=client)
            stub = '''import json, os, pathlib, signal, subprocess, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ["CALLS"], "a") as out:
    out.write(json.dumps([name, *args]) + "\\n")
ip, iid = os.environ["FIXTURE_IP"], os.environ["FIXTURE_INSTANCE"]
if name == "python3":
    if args and args[0].endswith("/lib/ssh_trust.py") and os.environ["AUTH_REFUSE"] == "1":
        sys.exit(1)
    os.execv(sys.executable, [sys.executable, *args])
elif name in ("aws", "bench-aws"):
    action = args[1]
    if action == "get-console-output":
        print(json.dumps({"Output": "256 " + os.environ["FIXTURE_FINGERPRINT"] + " host (ED25519)"}))
    elif action == "describe-instances":
        query = args[args.index("--query") + 1] if "--query" in args else ""
        if "--output" in args and args[args.index("--output") + 1] == "json":
            instances = [{"InstanceId": iid, "PublicIpAddress": ip}]
            if os.environ["AMBIGUOUS"] == "1": instances *= 2
            print(json.dumps({"Reservations": [{"Instances": instances}]}))
        elif "InstanceId" in query:
            print(iid + "\\t" + ip)
            if os.environ["AMBIGUOUS"] == "1": print(iid + "\\t" + ip)
        else: print(ip)
    elif action == "describe-security-groups": print("sg-fixture")
    elif action == "get-parameter": print("ami-fixture")
    elif action == "run-instances": print(iid)
elif name == "ssh-keyscan":
    print(ip + " ssh-ed25519 " + os.environ["FIXTURE_KEY"])
elif name == "ssh":
    if any("alive=" in arg for arg in args):
        print("alive=0\\ncells=1\\ndeadline=0\\nrundone=0")
    elif any("rundone=" in arg for arg in args): print("0\\nrundone=0")
    elif any("pgrep" in arg for arg in args): print("0")
    elif any("grep -a -E" in arg for arg in args): print("[cell 1/1] openai>openai: served")
elif name == "rsync":
    if args[-1].endswith("/busbar.json"):
        pathlib.Path(args[-1]).write_text('{"matrix":{"upstreams":{}}}')
elif name == "git":
    if "rev-parse" in args: print("a" * 40)
    elif "--contains" in args: print("origin/dev")
elif name == "sleep":
    if os.environ["AUTH_REFUSE"] == "1" and os.environ["CLIENT"] == "watch-orphans.sh":
        os.kill(os.getppid(), signal.SIGTERM)
'''
            for name in ["aws", "bench-aws", "ssh", "rsync", "ssh-keyscan", "sleep", "git", "python3"]:
                command = bins / name
                command.write_text("#!" + sys.executable + "\n" + stub)
                command.chmod(0o755)
            if client == "bench-dashboard.py":
                # Use the actual helper through the same mocked-python refusal seam as shell
                # clients. No dashboard parsing or fetch logic is replaced.
                probe = '''import importlib.util, os, pathlib, subprocess, sys
spec = importlib.util.spec_from_file_location("dashboard", sys.argv[1])
dashboard = importlib.util.module_from_spec(spec); spec.loader.exec_module(dashboard)
original = subprocess.run
def run(argv, *args, **kwargs):
    if len(argv) > 1 and argv[0] == sys.executable and str(argv[1]).endswith("/lib/ssh_trust.py"):
        argv = [str(pathlib.Path(os.environ["PATH"].split(os.pathsep)[0]) / "python3"), *argv[1:]]
    return original(argv, *args, **kwargs)
dashboard.subprocess.run = run
result = dashboard.remote_tail(os.environ["FIXTURE_IP"])
sys.exit(0 if result is not None else 1)
'''
                argv = [sys.executable, "-c", probe, str(source / client)]
            else:
                args = {"watch-busbar.sh": [IP], "watch-mutants.sh": [INSTANCE, IP],
                        "watch-orphans.sh": ["busbar"], "run-mutants-ec2.sh": []}[client]
                argv = ["bash", str(source / client), *args]
            result = subprocess.run(argv, env=environment, capture_output=True, text=True, timeout=10)
            recorded = [json.loads(line) for line in calls.read_text().splitlines()]
            pins = {str(path): path.read_text() for path in state.glob("*.known_hosts")}
            return result, recorded, pins

    def test_every_client_authenticates_before_strict_ssh_and_pulls(self):
        for client in CLIENTS:
            with self.subTest(client=client):
                result, calls, pins = self.run_client(client)
                self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
                authenticated = [i for i, row in enumerate(calls)
                                 if row[:3] == ["bench-aws", "ec2", "get-console-output"]]
                self.assertTrue(authenticated, "client never authenticated the EC2 host key")
                auth = authenticated[0]
                connections = [(i, row) for i, row in enumerate(calls) if row[0] in ["ssh", "rsync"]]
                self.assertTrue(connections)
                for i, row in connections:
                    self.assertGreater(i, auth)
                    options = " ".join(row)
                    self.assertIn("StrictHostKeyChecking=yes", options)
                    self.assertNotIn("StrictHostKeyChecking=no", options)
                    self.assertNotIn("UserKnownHostsFile=/dev/null", options)
                    self.assertTrue(any(path.replace(" ", "\\ ") in options or path in options for path in pins))
                self.assertEqual(list(pins.values()), [IP + " ssh-ed25519 " + KEY + "\n"])
                if client in ["watch-busbar.sh", "bench-dashboard.py"]:
                    self.assertFalse(any(row[:3] in [["bench-aws", "ec2", "run-instances"],
                                                     ["bench-aws", "ec2", "terminate-instances"]] for row in calls))

    def test_refused_authentication_prevents_every_connection_and_pull(self):
        for client in CLIENTS:
            with self.subTest(client=client):
                result, calls, pins = self.run_client(client, refuse=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any(row[0] in ["ssh", "rsync"] for row in calls), calls)
                self.assertFalse(pins)

    def test_ip_only_monitors_refuse_ambiguous_instance_identity(self):
        for client in ["watch-busbar.sh", "bench-dashboard.py"]:
            with self.subTest(client=client):
                result, calls, pins = self.run_client(client, ambiguous=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any(row[0] in ["ssh", "rsync"] for row in calls), calls)
                self.assertFalse(pins)


if __name__ == "__main__":
    unittest.main()
