#!/usr/bin/env bash
# Runs on the bench box before the engine starts, while the box still has real DNS (engine v1.2 cuts
# all outbound names to the mock once the gateway is set up). Plano's config generator runs
# `uv run` at every container start, which resolves and downloads its Python dependencies; under the
# catch-all DNS that download fails and the container exits. Build that environment once here, from
# the image's own uv.lock, into ./venv, which the manifest mounts at /app/.venv.
set -euo pipefail
cd "$(dirname "$0")"
IMAGE="$(python3 -c 'import json; print(json.load(open("definition.json"))["launch"]["image"])')"
rm -rf venv && mkdir -p venv
D=docker; docker info >/dev/null 2>&1 || D="sudo -n docker"
$D run --rm --entrypoint sh -v "$PWD/venv:/app/.venv" "$IMAGE" -c 'cd /app && uv sync --frozen'
test -x venv/bin/python
echo "plano prefetch: /app/.venv built from the image's uv.lock ($(du -sh venv | cut -f1))"
