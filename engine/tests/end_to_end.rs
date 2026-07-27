// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE ENGINE, END TO END, AS A TEST.
//
// Every other test in this crate exercises one module against fakes. That is why four finished
// modules - launch, rss, qualify and config_lint, 1,677 lines carrying 57 passing tests between
// them - could sit with ZERO callers while the suite reported 258 green: nothing anywhere asserted
// that a module is reachable from `otb run`. The only thing that could have caught it was a real
// run, and the real run has never completed.
//
// So this drives the actual binary against a real socket and asserts on the artifact it writes. It
// is deliberately not a unit test: it spawns `otb run`, which spawns pinned `otb loadgen` children
// of its own (`run::SweepProbe::spawn_pinned` re-invokes `current_exe`, which is why this cannot be
// done in-process - under `cargo test` the current exe is the TEST binary).
//
// Read `every_module_the_artifact_needs_is_reachable_from_a_real_run` as the gate. As each orphaned
// module is wired in, its assertion here goes from "documented as missing" to "required", and the
// module cannot fall back out of the graph without this failing.

// The crate denies unwrap/expect/panic so that a defect in a measurement can never abort a run
// mid-flight and lose the box-hours behind it. A test binary is the opposite case: if the fixture
// cannot bind a port or the binary cannot be found, the test must fail loudly and immediately rather
// than carry a degraded setup into an assertion and report something misleading about the engine.
// Scoped to this file only; the engine itself keeps the deny.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A stand-in that answers every dialect on the grid with a 200 and a small JSON body.
///
/// It is not the recording mock: it verifies nothing about which upstream dialect was spoken. It
/// exists so the ENGINE can be driven end to end without docker, and its only contract is to be a
/// well-framed HTTP/1.1 peer that keeps a connection alive, so the load generator's reuse path is
/// exercised rather than hidden behind a close after every response.
fn serve(stop: Arc<AtomicBool>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let addr = listener.local_addr().expect("read back the bound port");

    // A BLOCKING accept loop, not a polling one that sleeps between accepts to check a stop flag: a
    // burst of connections arriving at once (as the box-qualification window does) would queue
    // behind that sleep long enough for some requests to fail, and a benchmark fixture that cannot
    // serve the load being measured makes every number taken against it a measurement of the
    // fixture, not the engine.
    //
    // Nothing needs to poll for shutdown: the accept thread is detached, and the process exits when
    // the tests finish regardless of it.
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || handle(stream, stop));
        }
    });
    addr
}

fn handle(stream: TcpStream, stop: Arc<AtomicBool>) {
    stream.set_nodelay(true).ok();
    let mut reader = BufReader::new(stream.try_clone().expect("clone the stream for reading"));
    let mut out = stream;

    // One connection may carry many requests: the generator reuses them, and a reused connection is
    // exactly where a framing defect shows up.
    while !stop.load(Ordering::Relaxed) {
        let mut content_length = 0usize;
        let mut saw_request_line = false;

        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return, // peer hung up
                Ok(_) => {}
                Err(_) => return,
            }
            if !saw_request_line {
                saw_request_line = true;
            }
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break; // end of headers
            }
            if let Some(v) = trimmed.strip_prefix("Content-Length: ").or_else(|| trimmed.strip_prefix("content-length: ")) {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
        if !saw_request_line {
            return;
        }
        if content_length > 0 {
            let mut body = vec![0u8; content_length];
            if reader.read_exact(&mut body).is_err() {
                return;
            }
        }

        // A body shaped enough to be plausible, with an explicit Content-Length so framing is
        // unambiguous, and keep-alive so the connection can carry the next request.
        let body = br#"{"id":"e2e","object":"chat.completion","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n",
            body.len()
        );
        if out.write_all(head.as_bytes()).is_err() || out.write_all(body).is_err() || out.flush().is_err() {
            return;
        }
    }
}

fn otb() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_otb"))
}

fn unique_dir(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("otb-e2e-{name}-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the results dir");
    dir
}

/// The smallest manifest `otb run` will accept, written as the real thing: this is also a check that
/// the on-disk manifest shape the binary parses has not drifted from what the engine expects.
fn write_manifest(dir: &Path) -> PathBuf {
    let path = dir.join("definition.json");
    let body = r#"{
      "name": "e2e",
      "display": "End To End",
      "lang": "Rust",
      "class": "test fixture",
      "repo": "https://example.invalid/e2e",
      "port": 1,
      "path": "/v1/chat/completions",
      "model": "gpt-4o-mini",
      "auth": "dummy",
      "headers": [],
      "runtime": { "kind": "native", "proc_match": "e2e-fixture" },
      "egress": ["openai"],
      "config": []
    }"#;
    std::fs::write(&path, body).expect("write the manifest");
    path
}

/// Drive the real binary over a narrow grid and return the parsed snapshot.
fn run_engine(results: &Path, manifest: &Path, addr: SocketAddr) -> serde_json::Value {
    let output = Command::new(otb())
        .args([
            "run",
            manifest.to_string_lossy().as_ref(),
            &addr.to_string(), // the mock
            results.to_string_lossy().as_ref(),
            "1", // sweep_duration_s
        ])
        // The gateway's address normally comes from its declared port. The fixture binds an
        // ephemeral one, so it is overridden here - which is what the override exists for.
        .env("OTB_GATEWAY_ADDR", addr.to_string())
        // Keep the run small enough to be a test: one dialect, and a search range of a couple of
        // rungs. Without these the default is 36 cells x a peak search over [4, 512].
        .env("OTB_DIALECTS", "openai")
        .env("OTB_MIN_CONC", "1")
        .env("OTB_MAX_CONC", "2")
        .env("BENCH_ARCH", "e2e")
        .output()
        .expect("the otb binary must run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "otb run failed: status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status
    );

    let snapshot = results.join("e2e.json");
    assert!(
        snapshot.exists(),
        "otb run reported success but wrote no snapshot at {}\nstdout:\n{stdout}",
        snapshot.display()
    );
    let text = std::fs::read_to_string(&snapshot).expect("read the snapshot back");
    serde_json::from_str(&text).expect("the snapshot must be valid JSON")
}

// ── the gate ──────────────────────────────────────────────────────────────────────────────────────

/// A real run produces a real artifact. Nothing asserted this before.
#[test]
fn a_real_run_writes_a_snapshot_with_the_cell_it_measured() {
    let stop = Arc::new(AtomicBool::new(false));
    let addr = serve(Arc::clone(&stop));
    let results = unique_dir("writes");
    let manifest = write_manifest(&results);

    let snap = run_engine(&results, &manifest, addr);

    assert_eq!(snap["gateway"], "e2e");
    assert_eq!(snap["schema_version"], 1);
    assert_eq!(snap["arch"], "e2e");

    // The grid was one dialect, so exactly one cell, and the fixture answers 200, so it is served.
    let cell = &snap["matrix"]["upstreams"]["openai"]["cells"]["openai"];
    assert!(!cell.is_null(), "the openai/openai cell must appear in the artifact: {snap:#}");
    assert_eq!(cell["served"], true, "the fixture answered 200, so the cell is served: {cell:#}");
    assert_eq!(snap["matrix"]["served"], true);

    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&results);
}

/// THE REACHABILITY GATE.
///
/// Each assertion below names one module and the artifact field that proves the engine actually
/// called it during a real run. The four currently marked MISSING are the orphaned modules; as each
/// is wired in, move its line into the required block and this test starts holding it there.
///
/// The point is that a module falling out of the call graph becomes a RED TEST rather than a silent
/// hole that 258 passing unit tests cannot see.
#[test]
fn every_module_the_artifact_needs_is_reachable_from_a_real_run() {
    let stop = Arc::new(AtomicBool::new(false));
    let addr = serve(Arc::clone(&stop));
    let results = unique_dir("reachable");
    let manifest = write_manifest(&results);

    let snap = run_engine(&results, &manifest, addr);
    let cell = &snap["matrix"]["upstreams"]["openai"]["cells"]["openai"];

    // --- wired, and held here ---------------------------------------------------------------------
    // run + cell + probe: the cell exists and carries a verdict.
    assert!(!cell["served"].is_null(), "run/probe/cell must produce a verdict");
    // ingress: the path the request went to is recorded, and it is the dialect's path.
    assert_eq!(cell["path"], "/v1/chat/completions", "ingress must decide the path");
    // search + stats + gen + loadgen + http: a sweep ran and produced a perf block.
    assert!(!cell["perf"].is_null(), "search/gen/loadgen must produce perf for a served cell");
    // measurement + record + snapshot: it round-tripped to disk as valid JSON with our shape.
    assert_eq!(snap["gateway"], "e2e");

    // --- added_latency: WIRED ------------------------------------------------------------------------
    //
    // The fixture answers the gateway leg and the direct-to-mock leg on the SAME server (this rig has
    // no separate gateway process), so the two c=1 windows should both complete cleanly and the
    // difference should be a real, small number - asserted as a genuine measurement rather than just a
    // key, the same way box_qualify's observed_rps is: it is the one place this block proves the
    // concurrency-1 load path actually ran on both legs rather than merely being declared.
    let perf = &cell["perf"];
    assert!(
        perf["added_latency_p99_us"].as_f64().is_some(),
        "the added-latency group must publish a real added_latency_p99_us against a fixture that answers both legs: {perf:#}"
    );
    for field in ["added_latency_p50_us", "added_latency_p99_us", "gateway_c1_p99_us", "direct_c1_p99_us"] {
        assert!(perf.get(field).is_some(), "the added-latency group declares {field} and must publish it: {perf:#}");
    }

    // --- sustained_throughput: WIRED ------------------------------------------------------------------
    //
    // The grid here is narrowed to [1, 2] (see `run_engine`'s OTB_MIN_CONC/OTB_MAX_CONC), so this is
    // asserted as KEYS, not real numbers: `bisect_ceiling` over a two-rung range needs a real failure
    // to prove a ceiling, and this fixture never fails a request, so `SearchExhausted` is the honest
    // result. What this proves is that the group ran and filled its declared fields rather than being
    // skipped, exactly the standard the memory block above already holds itself to on this fixture.
    for field in ["rps_sustained_20ms", "rps_sustained_20ms_concurrency", "conc_at_sustained"] {
        assert!(perf.get(field).is_some(), "the sustained-throughput group declares {field} and must publish it: {perf:#}");
    }
    assert!(
        perf.get("sweep_sustained_20ms").is_some_and(|v| v.is_array()),
        "the sustained-throughput search's own probed rungs must travel as evidence: {perf:#}"
    );

    // --- streaming: WIRED --------------------------------------------------------------------------
    //
    // Every streaming number the board publishes is a difference between the stream through the
    // gateway and the same stream taken straight to the mock. Asserted as keys for the same reason as
    // memory: what this proves is that the group RAN and published what it declared. The fixture
    // answers with plain JSON rather than SSE frames, so the honest result here is an absence, and
    // the block must still say so rather than being missing.
    let stream = &cell["stream"];
    assert!(!stream.is_null(), "the streaming block must be published on a served cell: {cell:#}");
    for field in ["added_ttft_p50_us", "added_ttft_p99_us", "added_gap_p50_us", "added_gap_p99_us"] {
        assert!(
            stream.get(field).is_some(),
            "the streaming group declares {field} and must publish it, measured or absent: {stream:#}"
        );
    }
    // stream_served must never be a bare `false` derived from an absence: `false` is a claim about
    // the GATEWAY, and the reason we have here is a claim about the rig.
    assert_ne!(
        stream["stream_served"],
        serde_json::Value::Bool(false),
        "an absent streaming measurement must carry a status naming the reason, never a false that reads as 'this gateway does not stream': {stream:#}"
    );

    // --- qualify.rs: WIRED ------------------------------------------------------------------------
    //
    // The box is judged before the grid runs on it, and the verdict is published. No committed
    // snapshot has ever carried a non-null value here.
    //
    // On a first run there is no history, so the honest outcome is "seed" - this run IS the baseline.
    // Asserting "pass" would only hold on a machine with prior runs, i.e. never in a fresh checkout.
    let bq = &snap["rig"]["box_qualify"];
    assert!(!bq.is_null(), "the box-qualification verdict must be published: {snap:#}");
    assert_eq!(
        bq["outcome"], "seed",
        "with no history the run seeds the baseline rather than passing against nothing: {bq:#}"
    );
    assert_eq!(bq["baseline_samples"], 0, "a fresh results dir has no prior observations: {bq:#}");

    // THE ONE PLACE THIS TEST ASSERTS A REAL NUMBER, and it is the load path end to end: a pinned
    // `otb loadgen` child ran a window against the fixture and its rate came back through the
    // generator, the stats line, the point measurement and the record. Everything else here checks
    // that a field is PUBLISHED; this checks that the engine can actually measure something.
    assert!(
        bq["observed_rps"].as_f64().is_some_and(|v| v > 0.0),
        "the box observation must be a real rate, which is what proves the load path works: {bq:#}"
    );
    for field in ["outcome", "band_pct", "concurrency", "observed_rps", "baseline_rps", "drift_pct"] {
        assert!(bq.get(field).is_some(), "the verdict must publish {field}: {bq:#}");
    }

    // --- NOT YET WIRED: these are the holes this test exists to make visible ----------------------
    //
    // Each of these modules is complete and unit-tested and has ZERO callers. The assertions are
    // written as the artifact field they must fill, and are checked negatively FOR NOW so that this
    // test tells the truth about the engine as it stands today rather than pretending.
    //
    // When you wire a module, flip its assertion to the positive form on the right and delete it
    // from this block. If you wire it and forget, the negative assertion fails and tells you.

    // --- rss.rs: WIRED -----------------------------------------------------------------------------
    //
    // site/gen-data.mjs takes memory SOLELY from this per-cell window - "No fallback, and NO
    // per-gateway memory scalar" - so the window must be taken on every served cell or the board
    // publishes no memory for that gateway at all.
    //
    // The three numbers are asserted as KEYS, not as values, and that is deliberate. This fixture is
    // a thread inside the test process, not a separate process the manifest's declared identity can
    // find, and on a non-Linux host there is no /proc to read anyway. So on this machine the honest
    // result is three nulls, and asserting a number here would only pass on a Linux box with a real
    // gateway - i.e. it would be a test that cannot run where it is written. What this DOES prove is
    // that the memory group ran and filled its declared fields rather than being skipped.
    let memory = &cell["memory"];
    assert!(!memory.is_null(), "the memory window must be taken on a served cell: {cell:#}");
    for field in ["idle_rss_mib", "peak_rss_mib", "peak_rss_hwm_mib"] {
        assert!(
            memory.get(field).is_some(),
            "the memory group declares {field} and must publish it, measured or absent: {memory:#}"
        );
    }

    // record.rs config artifact: suite::flush builds the snapshot with ..Default::default(), so the
    // config every gw_config() exists to publish never lands. record.rs calls this "the whole point
    // (kills the class of bug where a chart is read against a config that was since overwritten)".
    assert!(
        snap["config"]["files"].as_object().is_none_or(|m| m.is_empty()),
        "the published config is now populated: assert it is non-empty instead"
    );

    // The build string names the ENGINE's version, not the gateway's. gw_version() exists in all 13
    // manifests to fill this, and nothing calls it: the board's reproducibility claim currently
    // identifies the wrong artifact.
    assert!(
        snap["build"].as_str().is_some_and(|b| b.starts_with("otb-engine")),
        "build still names the engine rather than the gateway under test: {:?}",
        snap["build"]
    );

    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&results);
}

/// THE CONFIG-NECESSITY GATE CAN ACTUALLY FAIL.
///
/// `config_lint` was the fourth orphaned module: complete, eleven tests, no callers. Wiring it in is
/// only worth anything if it can refuse something, and over the real corpus it currently cannot -
/// every one of the thirteen extracted manifests declares `config: []`, which the lint treats as the
/// ideal case rather than a violation, so it has nothing to judge. A gate that cannot fire is not a
/// gate, so this drives a manifest that DOES violate the standard and asserts the run is refused.
///
/// Refusing at the entry point is the point: after the run, the box-hours are spent and the tempting
/// fix is to publish anyway.
#[test]
fn a_manifest_that_fails_the_config_standard_is_refused_before_anything_is_measured() {
    let results = unique_dir("configlint");
    let manifest = results.join("definition.json");
    // Two settings claiming the same key: the manifest cannot say which one justifies the config the
    // gateway actually boots with.
    std::fs::write(
        &manifest,
        r#"{
          "name": "e2e-bad", "display": "Bad", "lang": "Rust", "class": "t",
          "repo": "https://example.invalid/e", "port": 1,
          "path": "/v1/chat/completions", "model": "gpt-4o-mini", "auth": "dummy", "headers": [],
          "runtime": { "kind": "native", "proc_match": "e2e-fixture" }, "egress": ["openai"],
          "config": [
            { "key": "listen.port", "reason": "rig_binding", "note": "8080" },
            { "key": "listen.port", "reason": "required_to_boot", "note": "9090" }
          ]
        }"#,
    )
    .expect("write the manifest");

    // A port nothing is listening on: if the gate works, the run is refused before any connection is
    // attempted, so the address never matters.
    let output = Command::new(otb())
        .args([
            "run",
            manifest.to_string_lossy().as_ref(),
            "127.0.0.1:9",
            results.to_string_lossy().as_ref(),
            "1",
        ])
        .env("OTB_DIALECTS", "openai")
        .output()
        .expect("the otb binary must run");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "a manifest that fails the standard must not be measured\n{stderr}");
    assert!(
        stderr.contains("config lint"),
        "the refusal must say which rule refused it, not just fail: {stderr}"
    );
    assert!(
        !results.join("e2e-bad.json").exists(),
        "nothing may be published for a manifest that was refused"
    );

    let _ = std::fs::remove_dir_all(&results);
}

/// The upstream flags are asserted, not measured. `suite.rs` hardcodes `configurable: true,
/// served: true` on every egress row, and `record.rs` names defaulting-to-true as claiming a
/// capability by omission. Held here so that wiring the launcher's typed failure into the record
/// has a test waiting for it.
#[test]
fn the_upstream_row_currently_asserts_health_it_never_measured() {
    let stop = Arc::new(AtomicBool::new(false));
    let addr = serve(Arc::clone(&stop));
    let results = unique_dir("upstream");
    let manifest = write_manifest(&results);

    let snap = run_engine(&results, &manifest, addr);
    let up = &snap["matrix"]["upstreams"]["openai"];

    // True here only because the fixture really did serve. The defect is that it would be true
    // regardless: nothing sets it from an observation.
    assert_eq!(up["served"], true);
    assert_eq!(up["configurable"], true);
    assert!(
        up["serve_error"].as_str().is_none_or(|s| s.is_empty()),
        "a healthy row carries no serve_error"
    );

    stop.store(true, Ordering::Relaxed);
    let _ = std::fs::remove_dir_all(&results);
}
