# Memory under sustained big-payload load

This suite measures **resident memory** while a gateway is held under sustained load with **large**
request bodies — the condition that separates a bounded working set from memory that grows and stays
pinned. It is run identically for every gateway in the field; the results feed
[`results/reports/`](../results/reports/) and the memory chart.

## What it records

For each gateway, on the same box, same mock, same load profile, one gateway at a time:

- **idle RSS** — right after the gateway first answers `200`, before any load.
- **peak RSS** — the highest resident memory sampled during sustained load.
- **post-load RSS** — sampled after load stops (see the settle window in `run.sh`): does memory
  release, or stay pinned at peak?

Memory is measured the **same way for every gateway** — the summed resident memory (VmRSS) of the
gateway's process tree, read from `/proc` (for containerized gateways, via the container's host PID).
We deliberately do **not** use `docker stats`, whose cgroup figure includes page cache and is not
comparable to a native process's RSS.

## Method choices

- **Large bodies at high concurrency, held for minutes.** A short small-payload run understates memory
  for any gateway that pools or pre-allocates buffers: the pools never fill and a boot-time
  `docker stats` snapshot is not a peak. Sustained large-payload load is what surfaces the real working
  set. Every gateway gets the same profile.
- **Each gateway runs its own default configuration.** We do not inject a throughput-tuned pool size
  into any gateway and then score it on memory; each is launched as it ships (see its
  `gateways/<name>/gateway.sh`).
- **A watchdog caps the run.** An unbounded gateway can exhaust the box, so `run.sh` kills the load the
  instant sampled memory crosses `CAP_MIB`, and container gateways run under a hard `--memory` cap so
  the kernel OOM-kills the container, never the host. A gateway that hits the cap is recorded as such,
  not hidden.

## Run it

```sh
GATEWAY=<name> memory/run.sh          # one gateway
# or run the whole field via ../run-all.sh
```

Knobs (env): `PSIZE` (payload bytes, default 150000), `CONC` (default 1500), `DUR` (seconds),
`CAP_MIB` (watchdog ceiling), `CORES` (gateway CPU pin). The measured numbers land in
`results/memory/<gateway>.json` and regenerate the chart.
