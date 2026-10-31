#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright (C) 2026 Busbar Inc and contributors

"""Stage and retain a private two-leg run; measurement remains the pinned otb engine."""

import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tarfile
from datetime import datetime, timezone

BASELINE_IMAGE = "getbusbar/busbar@sha256:1582c076d25483457c2c8d06e064f1171a0469337a41bf38ef36d29d4d05615d"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def image_ref(value):
    return isinstance(value, str) and re.fullmatch(
        r"[A-Za-z0-9._:/-]+@sha256:[0-9a-f]{64}", value
    )


def preflight(root, baseline, candidate, gateway, run_id, baseline_image):
    root = Path(root)
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", gateway) and gateway != "busbar",
            "candidate gateway must be a distinct safe directory name")
    require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", run_id), "unsafe RUN_ID")
    require(image_ref(baseline_image), "baseline image must name an immutable digest")
    require(baseline_image == BASELINE_IMAGE, "baseline must be the verified published 1.5.5 digest")
    sources = {}
    definitions = {}
    for leg, commit, directory in (("baseline", baseline, "busbar"), ("candidate", candidate, gateway)):
        require(re.fullmatch(r"[0-9a-f]{40}", commit), "source commits must be full SHAs")
        require(git(root, "cat-file", "-t", commit).strip() == b"commit", "source is not a commit")
        require(git(root, "branch", "-r", "--contains", commit).strip(), "source is not on a remote branch")
        paths = [f"gateways/{directory}"] + (["lib/rig.sh"] if leg == "baseline" else [])
        files = {}
        modes = {}
        for entry in git(root, "ls-tree", "-rz", commit, "--", *paths).split(b"\0"):
            if not entry:
                continue
            header, raw_path = entry.split(b"\t", 1)
            mode, kind, _ = header.split()
            name = raw_path.decode()
            require(mode in (b"100644", b"100755") and kind == b"blob", "non-file source entry")
            require(re.fullmatch(r"[A-Za-z0-9._/-]+", name) and ".." not in PurePosixPath(name).parts,
                    "unsafe source path")
            files[name] = sha(git(root, "show", f"{commit}:{name}"))
            modes[name] = int(mode, 8) & 0o777
        definition_path = f"gateways/{directory}/definition.json"
        require(definition_path in files, "gateway definition is missing")
        definition = json.loads(git(root, "show", f"{commit}:{definition_path}"))
        require(definition.get("name") == "busbar", "both legs must retain the busbar identity")
        require(definition.get("launch", {}).get("kind") == "docker", "pair requires Docker artifacts")
        for config in definition.get("config_files", []):
            output = config["output"]
            require(re.fullmatch(r"[A-Za-z0-9._/-]+", output) and
                    ".." not in PurePosixPath(output).parts and not output.startswith("/"),
                    "unsafe rendered configuration path")
        image = baseline_image if leg == "baseline" else definition["launch"]["image"]
        require(image_ref(image), "candidate image must name an immutable digest")
        definitions[leg] = definition
        sources[leg] = {"commit": commit, "directory": directory, "files": files, "image": image}
        sources[leg]["modes"] = modes
        sources[leg]["config_outputs"] = [config["output"] for config in definition.get("config_files", [])]
    for field in ("auth", "model", "path", "port", "matrix", "egress", "egress_models"):
        require(definitions["baseline"].get(field) == definitions["candidate"].get(field),
                f"candidate changes benchmark workload field: {field}")
    runner = git(root, "rev-parse", "HEAD").decode().strip()
    require(git(root, "branch", "-r", "--contains", runner).strip(), "push the orchestration commit first")
    directory = root / "results" / "pairs" / run_id
    directory.mkdir(parents=True, exist_ok=False)
    metadata = {"run_id": run_id, "runner_commit": runner, "sources": sources,
                "helper_sha256": sha(Path(__file__).read_bytes())}
    for leg, source in sources.items():
        paths = [f"gateways/{source['directory']}"] + (["lib/rig.sh"] if leg == "baseline" else [])
        with (directory / f"{leg}.tar").open("wb") as archive:
            subprocess.run(["git", "-C", str(root), "-c", "tar.umask=0022", "archive", "--format=tar", source["commit"], *paths],
                           stdout=archive, check=True)
        archived = {}
        with tarfile.open(directory / f"{leg}.tar") as archive:
            for member in archive:
                require(not member.name.startswith("/") and ".." not in PurePosixPath(member.name).parts,
                        "unsafe archive path")
                if member.isdir():
                    continue
                require(member.isfile() and member.name in source["files"], "unexpected archive member")
                require(member.name not in archived, "duplicate archive member")
                require(member.mode & 0o777 == source["modes"][member.name], "archive file mode changed")
                archived[member.name] = sha(archive.extractfile(member).read())
        require(archived == source["files"], "git archive transformed the pinned source files")
        source["archive_sha256"] = sha((directory / f"{leg}.tar").read_bytes())
    write_json(directory / "preflight.json", metadata)
    write_json(directory / "requested.json", metadata)


def metadata():
    return json.loads(Path("results/pair/preflight.json").read_text())


def hashes():
    return {name: sha(Path(name).read_bytes()) for name in ("otb", "bin/mock-arm64")}


def inspected(image):
    value = json.loads(subprocess.check_output(["docker", "image", "inspect", image]))[0]
    require(value["Architecture"] == "arm64", "image is not ARM64")
    require(any(d.endswith("@" + image.split("@", 1)[1]) for d in value.get("RepoDigests", [])),
            "pulled image digest is unverified")
    return {k: value.get(k) for k in ("Id", "RepoDigests", "Architecture", "Os")}


def prepare():
    meta = metadata()
    require(sha(Path(__file__).read_bytes()) == meta["helper_sha256"], "orchestration helper changed")
    for leg, source in meta["sources"].items():
        require(sha(Path(f"{leg}.tar").read_bytes()) == source["archive_sha256"], "source archive changed")
        for name, expected in source["files"].items():
            path = Path(name)
            require(not path.is_symlink() and sha(path.read_bytes()) == expected, "staged source changed")
            require(path.stat().st_mode & 0o777 == source["modes"][name], "staged source mode changed")
        directory = Path("gateways") / source["directory"]
        path = directory / "definition.json"
        original = path.read_bytes()
        (Path("results/pair") / f"{leg}.definition.original.json").write_bytes(original)
        definition = json.loads(original)
        definition["launch"]["image"] = source["image"]
        write_json(path, definition)
        subprocess.run(["docker", "pull", source["image"]], check=True)
        source["artifact"] = inspected(source["image"])
        source["effective_definition_sha256"] = sha(path.read_bytes())
    meta["rig"] = hashes()
    meta["instance_id"] = os.environ["BENCH_PAIR_INSTANCE_ID"]
    meta["ami"] = os.environ["BENCH_PAIR_AMI"]
    meta["host"] = {"uname": list(os.uname()), "os_release": Path("/etc/os-release").read_text(),
                    "cpuinfo": Path("/proc/cpuinfo").read_text(),
                    "meminfo": Path("/proc/meminfo").read_text()}
    meta["affinity"] = {key: os.environ[key] for key in ("CORES", "LOADCORES", "MOCKCORES")}
    meta["engine_commit"] = os.environ["BENCH_ENGINE_COMMIT"]
    meta["qualify_baseline"] = os.environ.get("OTB_QUALIFY_BASELINE", "")
    meta["dialects"] = os.environ["OTB_DIALECTS"]
    write_json(Path("results/pair/identity.json"), meta)


def leg_identity(leg, completed=False):
    meta = json.loads(Path("results/pair/identity.json").read_text())
    require(hashes() == meta["rig"], "engine or mock changed between legs")
    source = meta["sources"][leg]
    require(inspected(source["image"]) == source["artifact"], "gateway image changed")
    directory = Path("results/pair") / leg
    directory.mkdir(exist_ok=True)
    if not completed:
        write_json(directory / "started.json", {"at": datetime.now(timezone.utc).replace(microsecond=0).isoformat(),
                                               "instance_id": meta["instance_id"], "rig": hashes()})
        return
    gateway = Path("gateways") / source["directory"]
    definition = json.loads((gateway / "definition.json").read_text())
    require(sha((gateway / "definition.json").read_bytes()) == source["effective_definition_sha256"],
            "gateway definition changed during measurement")
    rendered = {}
    for config in definition.get("config_files", []):
        path = gateway / config["output"]
        data = path.read_bytes()
        destination = directory / "config" / config["output"]
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        rendered[config["output"]] = sha(data)
    write_json(directory / "completed.json", {"at": datetime.now(timezone.utc).isoformat(),
                                             "instance_id": meta["instance_id"], "rig": hashes(),
                                             "artifact": inspected(source["image"]), "config": rendered})


def verify(directory):
    directory = Path(directory)
    meta = json.loads((directory / "identity.json").read_text())
    requested = json.loads((directory / "requested.json").read_text())
    require(set(meta["sources"]) == set(requested["sources"]) == {"baseline", "candidate"},
            "both requested legs must be present")
    require(meta["run_id"] == requested["run_id"] and
            meta["runner_commit"] == requested["runner_commit"] and
            meta["helper_sha256"] == requested["helper_sha256"], "orchestration identity mismatch")
    require((directory / "run.status").read_text().strip() == "0", "pair did not finish cleanly")
    for leg in ("baseline", "candidate"):
        source = meta["sources"][leg]
        require(all(source[key] == value for key, value in requested["sources"][leg].items()),
                "gateway source identity mismatch")
        folder = directory / leg
        require((folder / "exit.status").read_text().strip() == "0", f"{leg} failed")
        started = json.loads((folder / "started.json").read_text())
        completed = json.loads((folder / "completed.json").read_text())
        require(started["instance_id"] == completed["instance_id"] == meta["instance_id"], "host mismatch")
        require(started["rig"] == completed["rig"] == meta["rig"], "instrument mismatch")
        require(completed["artifact"] == source["artifact"], "artifact mismatch")
        require(set(completed["config"]) == set(source["config_outputs"]), "configuration set mismatch")
        snapshots = list((folder / "snapshots").glob("result_busbar_*.json"))
        require(snapshots, f"{leg} has no snapshot")
        for path in snapshots:
            value = json.loads(path.read_text())
            require(value["gateway"] == "busbar" and value["build"] == source["image"], "snapshot identity mismatch")
            measured = datetime.fromisoformat(value["measured_at"].replace("Z", "+00:00"))
            require(datetime.fromisoformat(started["at"]) <= measured <=
                    datetime.fromisoformat(completed["at"]), "snapshot is outside its leg's interval")
            rig = value["rig"]
            require(rig["engine"]["commit"] == meta["engine_commit"] and not rig["engine"]["dirty"],
                    "snapshot engine mismatch")
            require(rig["mock"]["sha256"] == meta["rig"]["bin/mock-arm64"], "snapshot mock mismatch")
            cell = value["matrix"]["upstreams"]["openai"]["cells"]["openai"]
            require(cell["served"] and cell.get("perf") and cell["perf"].get("frontier"),
                    "OpenAI performance measurement is incomplete")
            for name, expected in completed["config"].items():
                require(sha((folder / "config" / name).read_bytes()) == expected, "configuration harvest mismatch")
                require(sha(value["config"]["files"][name].encode()) == expected, "snapshot configuration mismatch")
    print(f"retained both legs on {meta['instance_id']}; inspect metrics before judging performance")


if __name__ == "__main__":
    try:
        action, *args = sys.argv[1:]
        if action == "preflight":
            preflight(*args)
        elif action == "prepare":
            prepare()
        elif action == "begin":
            leg_identity(*args)
        elif action == "finish":
            leg_identity(*args, completed=True)
        elif action == "verify":
            verify(*args)
        else:
            raise ValueError("unknown pair action")
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError) as error:
        print(f"same-host pair: {error}", file=sys.stderr)
        sys.exit(1)
