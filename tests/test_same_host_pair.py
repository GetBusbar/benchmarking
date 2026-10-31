#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors

"""Controlled orchestration fixtures; no AWS, Docker or measuring workload."""

import copy
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("pair", ROOT / "lib/same_host_pair.py")
PAIR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PAIR)


class Evidence(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.meta = {"run_id": "fixture", "runner_commit": "r", "helper_sha256": "h",
                     "instance_id": "i-fixture", "engine_commit": "e",
                     "rig": {"otb": "engine", "bin/mock-arm64": "mock"}, "sources": {}}
        for leg in ("baseline", "candidate"):
            image = f"fixture/{leg}@sha256:" + "a" * 64
            self.meta["sources"][leg] = {"image": image, "config_outputs": ["config.yaml"],
                                           "artifact": {"Id": leg}}
            folder = self.root / leg
            (folder / "config").mkdir(parents=True)
            (folder / "snapshots").mkdir()
            (folder / "config/config.yaml").write_text("fixture: true\n")
            self.write(folder / "started.json", {"at": "2026-10-31T04:00:00+00:00",
                                                "instance_id": "i-fixture", "rig": self.meta["rig"]})
            self.write(folder / "completed.json", {
                "at": "2026-10-31T04:01:00+00:00", "instance_id": "i-fixture",
                "rig": self.meta["rig"], "artifact": {"Id": leg},
                "config": {"config.yaml": PAIR.sha(b"fixture: true\n")}})
            (folder / "exit.status").write_text("0\n")
            snapshot = {"gateway": "busbar", "build": image, "measured_at": "2026-10-31T04:00:00Z",
                        "rig": {"engine": {"commit": "e", "dirty": False}, "mock": {"sha256": "mock"}},
                        "config": {"files": {"config.yaml": "fixture: true\n"}},
                        "matrix": {"upstreams": {"openai": {"cells": {"openai": {
                            "served": True, "perf": {"frontier": [{"rps": 1}]}}}}}}}
            self.write(folder / "snapshots/result_busbar_fixture.json", snapshot)
        self.write(self.root / "identity.json", self.meta)
        requested = copy.deepcopy(self.meta)
        for source in requested["sources"].values():
            del source["artifact"]
        self.write(self.root / "requested.json", requested)
        (self.root / "run.status").write_text("0\n")

    @staticmethod
    def write(path, value):
        path.write_text(json.dumps(value))

    def test_complete_pair_with_second_precision_snapshot_is_retained(self):
        PAIR.verify(self.root)

    def test_missing_candidate_metadata_cannot_pass(self):
        del self.meta["sources"]["candidate"]
        self.write(self.root / "identity.json", self.meta)
        with self.assertRaisesRegex(ValueError, "both requested legs"):
            PAIR.verify(self.root)

    def test_missing_configuration_cannot_pass(self):
        path = self.root / "candidate/completed.json"
        completed = json.loads(path.read_text())
        completed["config"] = {}
        self.write(path, completed)
        with self.assertRaisesRegex(ValueError, "configuration set"):
            PAIR.verify(self.root)

    def test_changed_instrument_cannot_pass(self):
        path = self.root / "candidate/completed.json"
        completed = json.loads(path.read_text())
        completed["rig"]["otb"] = "different"
        self.write(path, completed)
        with self.assertRaisesRegex(ValueError, "instrument mismatch"):
            PAIR.verify(self.root)

    def test_corrupt_configuration_cannot_pass(self):
        (self.root / "candidate/config/config.yaml").write_text("different")
        with self.assertRaisesRegex(ValueError, "configuration harvest"):
            PAIR.verify(self.root)

    def test_stale_snapshot_cannot_pass(self):
        path = self.root / "candidate/snapshots/result_busbar_fixture.json"
        snapshot = json.loads(path.read_text())
        snapshot["measured_at"] = "2026-10-30T04:00:00Z"
        self.write(path, snapshot)
        with self.assertRaisesRegex(ValueError, "outside its leg"):
            PAIR.verify(self.root)


class ControlFlow(unittest.TestCase):
    def test_real_pair_function_aborts_after_baseline_or_log_failure(self):
        source = (ROOT / "run-on-ec2.sh").read_text()
        function = "pair_run() {\n" + source.split("  pair_run() {\n", 1)[1].split("\n  pair_run\n", 1)[0]
        for baseline_exit, broken_log, expected in ((0, False, 0), (13, False, 13), (0, True, 125)):
            with self.subTest(baseline_exit=baseline_exit, broken_log=broken_log), tempfile.TemporaryDirectory() as name:
                root = Path(name)
                (root / "bin").mkdir()
                (root / "bin/python3").write_text("#!/bin/sh\nexit 0\n")
                (root / "bin/python3").chmod(0o755)
                if broken_log:
                    (root / "bin/tee").write_text("#!/bin/sh\ncat >/dev/null\nexit 1\n")
                    (root / "bin/tee").chmod(0o755)
                (root / "otb").write_text(
                    '#!/bin/sh\n[ "$1" = run ] || exit 0\n'
                    'echo "$OTB_RUN_ID" >> events\n'
                    'case "$OTB_RUN_ID" in *-baseline) exit "$BASELINE_EXIT";; esac\nexit 0\n')
                (root / "otb").chmod(0o755)
                env = {**os.environ, "PATH": str(root / "bin") + os.pathsep + os.environ["PATH"],
                       "BASELINE_EXIT": str(baseline_exit), "RUN_ID": "fixture", "CORES": "0-3",
                       "LOADCORES": "4-9", "BENCH_PAIR_CANDIDATE_GATEWAY": "candidate"}
                result = subprocess.run(["bash", "-c", function + "\npair_run\n"], cwd=root,
                                        env=env, capture_output=True, text=True, timeout=20)
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                events = (root / "events").read_text().splitlines()
                self.assertEqual(events, ["fixture-baseline", "fixture-candidate"] if expected == 0
                                 else ["fixture-baseline"])


class Archives(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "gateways").mkdir()
        shutil.copytree(ROOT / "gateways/busbar", self.root / "gateways/busbar")
        shutil.copytree(ROOT / "gateways/busbar", self.root / "gateways/candidate")
        path = self.root / "gateways/candidate/definition.json"
        definition = json.loads(path.read_text())
        definition["launch"]["image"] = "fixture/candidate@sha256:" + "b" * 64
        path.write_text(json.dumps(definition))
        (self.root / "lib").mkdir()
        shutil.copyfile(ROOT / "lib/rig.sh", self.root / "lib/rig.sh")
        (self.root / "lib/rig.sh").chmod(0o755)
        self.git("init", "-q")
        self.git("config", "core.attributesFile", "/dev/null")
        self.git("add", "gateways", "lib")
        self.commit()

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args], stderr=subprocess.STDOUT)

    def commit(self):
        self.git("-c", "user.name=Matthew Jackson", "-c",
                 "user.email=1085847+MattJackson@users.noreply.github.com", "-c", "commit.gpgsign=false",
                 "commit", "-qm", "Controlled archive fixture")
        self.commit_id = self.git("rev-parse", "HEAD").decode().strip()
        self.git("update-ref", "refs/remotes/origin/fixture", self.commit_id)

    def prepare(self, run_id, image=PAIR.BASELINE_IMAGE):
        PAIR.preflight(str(self.root), self.commit_id, self.commit_id, "candidate", run_id, image)

    def test_archives_preserve_the_pinned_file_modes_and_bytes(self):
        self.prepare("modes")
        with tarfile.open(self.root / "results/pairs/modes/baseline.tar") as archive:
            self.assertEqual(archive.getmember("lib/rig.sh").mode & 0o777, 0o755)
            self.assertEqual(archive.getmember("gateways/busbar/definition.json").mode & 0o777, 0o644)

    def test_export_ignore_cannot_silently_change_the_source_bundle(self):
        (self.root / ".gitattributes").write_text("gateways/candidate/config.gen.yaml.tmpl export-ignore\n")
        self.git("add", ".gitattributes")
        self.commit()
        with self.assertRaisesRegex(ValueError, "transformed the pinned source"):
            self.prepare("ignored")

    def test_another_release_digest_cannot_be_labeled_the_baseline(self):
        with self.assertRaisesRegex(ValueError, "verified published 1.5.5"):
            self.prepare("wrong-baseline", "getbusbar/busbar@sha256:" + "c" * 64)


if __name__ == "__main__":
    unittest.main()
