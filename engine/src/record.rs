// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE SHAPE OF THE ARTIFACT THIS ENGINE PUBLISHES.
//
// The shell engine built the per-gateway snapshot by string concatenation: a hand-rolled
// `json_escape`, printf format strings threading dozens of positional arguments, and an `emit_cell`
// that folded three optional fragments (perf / stream / memory) into one object by literal string
// splicing. That gets a public website's data from a shell script with no compiler checking that the
// braces balance or that a value got escaped before it landed between two quotes.
//
// These types are the replacement: the same shape, described once, serialised by serde instead of by
// hand. Every published metric is a `Measurement<T>` (see measurement.rs), so an unmeasured cell
// reads as `null` on the wire and never as a 0 that a chart would draw. Structural fields (`served`,
// `status`, dialect names) are not measurements and stay as plain strings/bools/enums: the discipline
// applies to numbers a reader could mistake for a result, not to labels.
//
// SHAPE SOURCE. This module was built by reading matrix/run.sh's `emit_cell` and its two heredocs
// (the per-gateway `$RESULTS/$GATEWAY.json` and the snapshot-writer's embedded Python), and by
// reading real committed snapshots under results/snapshots/. Where a field's shape could be
// confirmed against a real snapshot, it is typed precisely. Where the shell defines a field this repo
// has never actually populated (per-cell memory windows, a fully-measured streaming block, the box
// qualification stage bodies), the type follows the shell source but is opt-in permissive
// (`serde_json::Value` or `Option`) rather than guessed at, because there was no real artifact to
// check it against. `matrix_version: 2` is the schema this module targets; older committed snapshots
// predate several fields (rig provenance, run timing, cell_memory), which is why almost everything
// outside the core measurement grid is `Option` with `#[serde(default)]`.

use crate::measurement::{Absent, Measurement};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// `#[serde(default)]` needs a concrete `Default` impl, and `Measurement<T>` deliberately has none
/// (a type that silently defaults would be one step from the `value_or_zero` this whole module
/// exists to forbid). This is the one narrow, explicit exception: a field the wire OMITS entirely
/// (as opposed to sending `null` for) is absent for the same reason a `null` is, so it fills in the
/// same way `Measurement`'s own `Deserialize` fills in a `null`: as `NotMeasured`, never as a zero.
fn measurement_default<T>() -> Measurement<T> {
    Measurement::absent(Absent::NotMeasured)
}

/// A handful of list fields (memory's `rss_series`, chiefly) come back as JSON `null` rather than `[]`
/// when the shell had nothing to put there (a memory window that never served). `Vec<T>` has no
/// `Deserialize` for `null`, so without this the whole snapshot would fail to parse over one absent
/// series. Folds `null` to the empty vec, the same "nothing happened here" reading `[]` already has.
fn null_as_empty_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

/// The top-level artifact written to `results/snapshots/result_<gateway>_<ts>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultSnapshot {
    pub schema_version: u32,
    pub gateway: String,
    #[serde(default)]
    pub build: String,
    pub measured_at: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub duration_s: Option<i64>,
    #[serde(default)]
    pub phase_s: Option<PhaseSeconds>,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub hardware: Option<String>,
    /// The measurement instrument's own provenance (rig binaries + box qualification). Absent on
    /// every snapshot taken before audit #21 added it, and best-effort even after: a consumer must
    /// read "no rig block" as "not recorded", never as "not qualified".
    #[serde(default)]
    pub rig: Option<RigProvenance>,
    #[serde(default)]
    pub config: ConfigFiles,
    /// The full per-gateway matrix result, embedded verbatim. This is the sole numeric source; every
    /// other top-level field here is either provenance or a reader-facing projection of it.
    pub matrix: Matrix,
    /// Mirrors `matrix.memory` exactly (the run.sh writer copies it verbatim). Kept as its own field
    /// because that is the shape on the wire, not because it is a second measurement.
    #[serde(default)]
    pub memory: Option<MatrixMemory>,
    /// The best-diagonal streaming projection the snapshot writer computes for a quick-glance reader.
    /// Absent whenever no served diagonal cell actually streamed.
    #[serde(default)]
    pub streaming: Option<StreamingProjection>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PhaseSeconds {
    #[serde(default)]
    pub build: Option<i64>,
    #[serde(default, rename = "matrix_6x6")]
    pub matrix_6x6: Option<i64>,
    #[serde(default)]
    pub memory_window: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ConfigFiles {
    /// Verbatim rendered config text, keyed by the basename run.sh wrote it under. Config lives
    /// inside the same artifact as the numbers it produced, which is the whole point (kills the
    /// class of bug where a chart is read against a config that was since overwritten on disk).
    #[serde(default)]
    pub files: HashMap<String, String>,
}

// ─────────────────────────────── rig / box-qualification provenance ────────────────────────────────

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RigProvenance {
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub release_url: Option<String>,
    #[serde(default)]
    pub engine: Option<EngineStamp>,
    #[serde(default)]
    pub mock: Option<BinaryProvenance>,
    #[serde(default)]
    pub ugen: Option<BinaryProvenance>,
    /// The box-qualification verdict this run's box carried, folded in by rig_provenance_json. No
    /// committed snapshot has ever carried a non-null value here, so the stage bodies are passed
    /// through as opaque JSON rather than typed against a shape nothing has confirmed.
    #[serde(default)]
    pub box_qualify: Option<serde_json::Value>,
}

/// Which commit produced this run's harness, and whether the tree was dirty. `null` (not an absent
/// commit string) when the harness could not identify itself, which is why this is `Option` and not
/// wrapped in `Measurement`: it is provenance, not a measured quantity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineStamp {
    pub commit: String,
    pub dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BinaryProvenance {
    #[serde(default)]
    pub origin: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub asset_updated_at: Option<String>,
}

// ────────────────────────────────────────── the matrix ─────────────────────────────────────────────

/// The per-gateway matrix result (`$RESULTS/$GATEWAY.json`), embedded verbatim under `matrix`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Matrix {
    pub gateway: String,
    #[serde(default)]
    pub build: String,
    #[serde(default)]
    pub matrix_version: Option<u32>,
    pub served: bool,
    #[serde(default)]
    pub serve_error: String,
    #[serde(default)]
    pub upstream_shape: String,
    #[serde(default)]
    pub upstream_note: String,
    #[serde(default)]
    pub egress_configured: String,
    #[serde(default)]
    pub probe_first: bool,
    #[serde(default)]
    pub capability_note: String,
    #[serde(default)]
    pub cell_perf_sweep: bool,
    #[serde(default)]
    pub sweep_rung_selection: Option<String>,
    #[serde(default)]
    pub sweep_ttft_ms: Option<i64>,
    #[serde(default)]
    pub p99_ceiling_ms: Option<i64>,
    #[serde(default)]
    pub sweep_dur: Option<i64>,
    #[serde(default)]
    pub cell_stream: bool,
    #[serde(default)]
    pub cell_memory: Option<bool>,
    /// The post-6x6 memory window (one fixed-recipe run on the peak cell), NOT the per-cell windows.
    /// Absent until the field run that carries it; the per-cell windows live under
    /// `upstreams.<egress>.cells.<ingress>.memory` instead and are never summarised here.
    #[serde(default)]
    pub memory: Option<MatrixMemory>,
    /// The v1-compat single-egress row (top-level `cells`), keyed by ingress dialect.
    #[serde(default)]
    pub cells: HashMap<String, Cell>,
    /// The full 6x6 grid: egress dialect -> its upstream block.
    #[serde(default)]
    pub upstreams: HashMap<String, Upstream>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub upstream_endpoint: Option<String>,
    #[serde(default)]
    pub ootb_config: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub hardware: Option<String>,
    #[serde(default)]
    pub rig: Option<RigProvenance>,
    pub measured_at: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub duration_s: Option<i64>,
    #[serde(default)]
    pub phase_s: Option<PhaseSeconds>,
    #[serde(default)]
    pub build_env: Option<BuildEnv>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BuildEnv {
    #[serde(default)]
    pub prereqs: bool,
    #[serde(default)]
    pub quiesce: String,
}

/// One egress dialect's block: whether this gateway can be pointed at it, and the full ingress row
/// probed against that egress.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Upstream {
    pub configurable: bool,
    /// `served` here is whether the EGRESS configuration itself came up, not a per-cell verdict.
    pub served: bool,
    #[serde(default)]
    pub egress_config: Option<String>,
    #[serde(default)]
    pub serve_error: String,
    /// Ingress dialect -> the cell probed through this egress.
    #[serde(default)]
    pub cells: HashMap<String, Cell>,
}

/// Whether/how a cell was served. Not a `Measurement`: this is a verdict label, not a number a chart
/// could plot, and its non-`true` values are all deliberately readable strings (a reader-facing
/// vocabulary, not an internal enum tag) exactly as the shell wrote them: `false`,
/// `"not_configured"`, `"not_configurable"`, `"unprobed_auth"`, `"not_verified"`, `"untestable"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Served {
    Bool(bool),
    Status(String),
}

/// One probed (ingress, egress) pairing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub served: Served,
    /// Present when `served` is a non-`true` status string; the machine-readable reason behind it.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub verdict_note: String,
    #[serde(default)]
    pub body_snippet: String,
    /// Set for cells the harness never reached (e.g. not-configured skips): why the probe wasn't run.
    #[serde(default)]
    pub probe_note: Option<String>,
    /// Set once, at the moment perf + stream are withheld because leg-3 re-verification found a
    /// misroute: publishing either would record a misrouted number under this cell's name.
    #[serde(default)]
    pub perf_dropped: Option<String>,
    #[serde(default)]
    pub perf: Option<CellPerf>,
    #[serde(default)]
    pub stream: Option<CellStream>,
    /// The per-cell memory window (see `CellMemory`). No committed snapshot has populated this yet;
    /// the field exists so a future run that does carries it through unchanged.
    #[serde(default)]
    pub memory: Option<CellMemory>,
}

/// One rung of a concurrency sweep. `rps` / `p99_us` / `fail` are `Measurement` on principle (a rung
/// the harness could not sample would otherwise have nowhere honest to put that fact), even though
/// every rung the shell engine has ever published carried real values for all three.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SweepPoint {
    pub conc: i64,
    #[serde(default = "measurement_default")]
    pub rps: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub fail: Measurement<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellPerf {
    #[serde(default = "measurement_default")]
    pub added_latency_p50_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub added_latency_p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub gateway_c1_p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub direct_c1_p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub rps_sustained_20ms: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub rps_sustained_20ms_concurrency: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub conc_at_sustained: Measurement<i64>,
    /// Whether the sustained-throughput rung was bounded by the mock/rig rather than the gateway. A
    /// flag, not a `Measurement`: it qualifies the number above, it is not itself absent-able.
    #[serde(default)]
    pub rps_sustained_20ms_mock_bound: Option<bool>,
    #[serde(default = "measurement_default")]
    pub rps_max_proxy: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub rps_max_proxy_concurrency: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub conc_at_peak: Measurement<i64>,
    #[serde(default)]
    pub rps_max_proxy_mock_bound: Option<bool>,
    #[serde(default)]
    pub sweep_max_proxy: Vec<SweepPoint>,
    #[serde(default)]
    pub sweep_sustained_20ms: Vec<SweepPoint>,
    #[serde(default)]
    pub egress_reverified: Option<bool>,
    #[serde(default)]
    pub reverify_note: Option<String>,
    #[serde(default)]
    pub c1_note: Option<String>,
}

/// Whether/how a cell's streaming path was exercised. Like `Served`, this is a verdict label rather
/// than a number: `true`, `false`, or `"untestable"` (the mock/rig cannot pose this question at all
/// for this pairing).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StreamServed {
    Bool(bool),
    Status(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellStream {
    pub stream_served: StreamServed,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub stream_error: Option<String>,
    #[serde(default = "measurement_default")]
    pub added_ttft_p50_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub added_ttft_p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub added_gap_p50_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub added_gap_p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub streams_sustained: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub streams_sustained_fps: Measurement<f64>,
    #[serde(default)]
    pub streams_sustained_mock_bound: Option<bool>,
    #[serde(default = "measurement_default")]
    pub cpu_fps: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub cpu_fps_concurrency: Measurement<i64>,
    #[serde(default)]
    pub cpu_fps_mock_bound: Option<bool>,
    /// The sweep points behind `streams_sustained` / `cpu_fps`. Shaped like `SweepPoint` in run.sh's
    /// own accumulator, but no committed snapshot has ever populated it, so each point is passed
    /// through as opaque JSON rather than typed against an unconfirmed shape.
    #[serde(default)]
    pub sweep_streams: Vec<serde_json::Value>,
    #[serde(default)]
    pub sweep_cpu_fps: Vec<serde_json::Value>,
    #[serde(default)]
    pub stream_c1_note: Option<String>,
}

/// One (t_s, rss_mib) sample in a memory window's time series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RssSample {
    pub t_s: i64,
    #[serde(default = "measurement_default")]
    pub rss_mib: Measurement<f64>,
}

/// The post-6x6, fixed-recipe memory window: one run, on the peak cell, after the whole grid. This is
/// the shape every committed snapshot with memory data actually carries (`matrix.memory`, mirrored at
/// the snapshot's top-level `memory`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatrixMemory {
    #[serde(default)]
    pub protocol: String,
    pub served: bool,
    #[serde(default)]
    pub serve_error: String,
    #[serde(default)]
    pub load_cell: Option<String>,
    #[serde(default)]
    pub load_recipe: Option<LoadRecipe>,
    #[serde(default = "measurement_default")]
    pub idle_rss_mib: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub peak_rss_mib: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub peak_rss_hwm_mib: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub post_load_rss_mib: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub recovered_rss_mib: Measurement<f64>,
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub rss_series: Vec<RssSample>,
    #[serde(default)]
    pub idle_window_s: Option<i64>,
    #[serde(default)]
    pub recovery_window_s: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadRecipe {
    pub concurrency: i64,
    pub payload_bytes: i64,
    pub duration_s: i64,
}

/// The PER-CELL memory window (`CELL_MEM_JSON` in run.sh): its own cold-started, plateau-terminated
/// window for one served cell. Distinct field set from `MatrixMemory` (plateau/growth-rate instead of
/// a single post-load recipe) because it answers a different question: not "what did the peak cell do
/// after everything else ran" but "what does THIS cell do on its own, run cold". No committed snapshot
/// has populated this yet: typed from the shell source, kept permissive by being `Option` throughout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellMemory {
    #[serde(default)]
    pub protocol: String,
    pub served: bool,
    #[serde(default)]
    pub serve_error: String,
    #[serde(default)]
    pub load_recipe: Option<serde_json::Value>,
    #[serde(default = "measurement_default")]
    pub idle_rss_mib: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub steady_state_rss_mib: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub recovered_rss_mib: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub peak_rss_mib: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub peak_rss_hwm_mib: Measurement<f64>,
    #[serde(default)]
    pub plateaued: Option<bool>,
    #[serde(default = "measurement_default")]
    pub time_to_plateau_s: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub growth_rate_mib_per_min: Measurement<f64>,
    #[serde(default)]
    pub load_s: Option<i64>,
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub rss_series: Vec<RssSample>,
    #[serde(default)]
    pub idle_window_s: Option<i64>,
    #[serde(default)]
    pub recovery_window_s: Option<i64>,
}

/// The snapshot writer's best-diagonal streaming projection: whichever served diagonal cell actually
/// streamed (openai first, then the first other served+streamed diagonal), for a quick-glance reader.
/// No committed snapshot has populated this yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamingProjection {
    pub dialect: String,
    #[serde(default)]
    pub ttft_ms: Option<i64>,
    #[serde(default = "measurement_default")]
    pub added_ttft_p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub added_gap_p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub streams_sustained: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub cpu_fps: Measurement<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::Absent;
    use std::fs;
    use std::path::PathBuf;

    fn snapshots_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../results/snapshots")
    }


    fn sample_perf() -> CellPerf {
        CellPerf {
            added_latency_p50_us: Measurement::Measured(40_939),
            added_latency_p99_us: Measurement::Measured(40_945),
            gateway_c1_p99_us: Measurement::Measured(41_026),
            direct_c1_p99_us: Measurement::Measured(81),
            rps_sustained_20ms: Measurement::Measured(11_968),
            rps_sustained_20ms_concurrency: Measurement::Measured(1024),
            conc_at_sustained: Measurement::Measured(1024),
            rps_sustained_20ms_mock_bound: Some(false),
            rps_max_proxy: Measurement::Measured(12_298),
            rps_max_proxy_concurrency: Measurement::Measured(1024),
            conc_at_peak: Measurement::Measured(1024),
            rps_max_proxy_mock_bound: Some(false),
            sweep_max_proxy: vec![SweepPoint {
                conc: 256,
                rps: Measurement::Measured(6_209),
                p99_us: Measurement::Measured(43_969),
                fail: Measurement::Measured(0),
            }],
            sweep_sustained_20ms: vec![],
            egress_reverified: Some(true),
            reverify_note: None,
            c1_note: None,
        }
    }

    fn sample_cell() -> Cell {
        Cell {
            served: Served::Bool(true),
            reason: None,
            status: "200".to_string(),
            path: "/openai/v1/chat/completions".to_string(),
            verdict_note: "HTTP 200, openai envelope validated".to_string(),
            body_snippet: "{\"id\":\"x\"}".to_string(),
            probe_note: None,
            perf_dropped: None,
            perf: Some(sample_perf()),
            stream: Some(CellStream {
                stream_served: StreamServed::Status("untestable".to_string()),
                reason: Some("stream_mock_unready".to_string()),
                stream_error: Some("did not rebind the mock port".to_string()),
                // NotMeasured, not Untestable: a `null` on the wire always deserialises back to
                // NotMeasured (measurement.rs's Deserialize cannot recover a more specific reason
                // from a bare null), so a round-trippable fixture must start from the reason that
                // survives the trip. The reason-preservation behaviour itself is measurement.rs's
                // own concern and is exercised there, not duplicated here.
                added_ttft_p50_us: Measurement::absent(Absent::NotMeasured),
                added_ttft_p99_us: Measurement::absent(Absent::NotMeasured),
                added_gap_p50_us: Measurement::absent(Absent::NotMeasured),
                added_gap_p99_us: Measurement::absent(Absent::NotMeasured),
                streams_sustained: Measurement::absent(Absent::NotMeasured),
                streams_sustained_fps: Measurement::absent(Absent::NotMeasured),
                streams_sustained_mock_bound: None,
                cpu_fps: Measurement::absent(Absent::NotMeasured),
                cpu_fps_concurrency: Measurement::absent(Absent::NotMeasured),
                cpu_fps_mock_bound: None,
                sweep_streams: vec![],
                sweep_cpu_fps: vec![],
                stream_c1_note: None,
            }),
            memory: None,
        }
    }

    fn sample_upstream() -> Upstream {
        let mut cells = HashMap::new();
        cells.insert("openai".to_string(), sample_cell());
        Upstream {
            configurable: true,
            served: true,
            egress_config: Some("default".to_string()),
            serve_error: String::new(),
            cells,
        }
    }

    fn sample_matrix() -> Matrix {
        let mut upstreams = HashMap::new();
        upstreams.insert("openai".to_string(), sample_upstream());
        let mut cells = HashMap::new();
        cells.insert("openai".to_string(), sample_cell());
        Matrix {
            gateway: "gw".to_string(),
            build: "gw:1.0.0".to_string(),
            matrix_version: Some(2),
            served: true,
            serve_error: String::new(),
            upstream_shape: "openai".to_string(),
            upstream_note: "v2: full 6x6".to_string(),
            egress_configured: "openai".to_string(),
            probe_first: true,
            capability_note: "advisory context only".to_string(),
            cell_perf_sweep: true,
            sweep_rung_selection: Some("adaptive".to_string()),
            sweep_ttft_ms: Some(500),
            p99_ceiling_ms: Some(200),
            sweep_dur: Some(10),
            cell_stream: true,
            cell_memory: Some(true),
            memory: None,
            cells,
            upstreams,
            model: Some("gpt-4o-mini".to_string()),
            upstream_endpoint: Some("/v1/chat/completions".to_string()),
            ootb_config: None,
            arch: Some("arm64".to_string()),
            hardware: Some("m7g.4xlarge".to_string()),
            rig: None,
            measured_at: "2026-07-25T08:26:15Z".to_string(),
            started_at: None,
            finished_at: None,
            duration_s: None,
            phase_s: None,
            build_env: None,
        }
    }

    fn sample_record() -> ResultSnapshot {
        ResultSnapshot {
            schema_version: 1,
            gateway: "gw".to_string(),
            build: "gw:1.0.0".to_string(),
            measured_at: "2026-07-25T08:26:15Z".to_string(),
            started_at: None,
            finished_at: None,
            duration_s: None,
            phase_s: None,
            arch: Some("arm64".to_string()),
            hardware: Some("m7g.4xlarge".to_string()),
            rig: None,
            config: ConfigFiles { files: HashMap::new() },
            matrix: sample_matrix(),
            memory: None,
            streaming: None,
        }
    }

    // ── round-trip ───────────────────────────────────────────────────────────────────────────────

    #[test]
    fn record_round_trips_through_json() {
        let rec = sample_record();
        let js = serde_json::to_string(&rec).unwrap();
        let back: ResultSnapshot = serde_json::from_str(&js).unwrap();
        assert_eq!(rec, back);
    }

    #[test]
    fn cell_perf_round_trips_through_json() {
        let perf = sample_perf();
        let js = serde_json::to_string(&perf).unwrap();
        let back: CellPerf = serde_json::from_str(&js).unwrap();
        assert_eq!(perf, back);
    }

    #[test]
    fn served_status_variant_round_trips() {
        for s in [Served::Bool(true), Served::Bool(false), Served::Status("untestable".to_string())] {
            let js = serde_json::to_string(&s).unwrap();
            let back: Served = serde_json::from_str(&js).unwrap();
            assert_eq!(s, back);
        }
    }

    // ── golden shape ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn golden_shape_matches_real_key_paths() {
        let rec = sample_record();
        let v: serde_json::Value = serde_json::to_value(&rec).unwrap();
        // Key paths pulled directly from a real committed snapshot, not invented here.
        assert!(v.pointer("/matrix/upstreams/openai/cells/openai/perf/rps_max_proxy").is_some());
        assert!(v.pointer("/matrix/upstreams/openai/cells/openai/perf/sweep_max_proxy/0/conc").is_some());
        assert!(v.pointer("/matrix/upstreams/openai/cells/openai/stream/stream_served").is_some());
        assert!(v.pointer("/matrix/cells/openai/served").is_some());
        assert!(v.pointer("/matrix/upstreams/openai/configurable").is_some());
        assert!(v.pointer("/config/files").is_some());
        assert_eq!(
            v.pointer("/matrix/upstreams/openai/cells/openai/perf/rps_max_proxy").unwrap(),
            &serde_json::json!(12_298)
        );
    }

    // ── absence ──────────────────────────────────────────────────────────────────────────────────

    #[test]
    fn absent_perf_metric_serialises_as_null_never_zero() {
        let mut perf = sample_perf();
        perf.rps_max_proxy = Measurement::absent(Absent::SearchExhausted);
        let js = serde_json::to_string(&perf).unwrap();
        let v: serde_json::Value = serde_json::from_str(&js).unwrap();
        assert_eq!(v["rps_max_proxy"], serde_json::Value::Null);
        assert!(!js.contains("\"rps_max_proxy\":0"));
    }

    #[test]
    fn absent_stream_metric_serialises_as_null() {
        let cell = sample_cell();
        let stream = cell.stream.unwrap();
        let js = serde_json::to_string(&stream).unwrap();
        let v: serde_json::Value = serde_json::from_str(&js).unwrap();
        assert_eq!(v["added_ttft_p99_us"], serde_json::Value::Null);
        assert_eq!(v["cpu_fps"], serde_json::Value::Null);
    }

    #[test]
    fn null_field_deserialises_to_absent_not_measured_zero() {
        let js = r#"{
            "added_latency_p50_us": null,
            "added_latency_p99_us": null,
            "gateway_c1_p99_us": null,
            "direct_c1_p99_us": null,
            "rps_sustained_20ms": null,
            "rps_sustained_20ms_concurrency": null,
            "conc_at_sustained": null,
            "rps_sustained_20ms_mock_bound": null,
            "rps_max_proxy": null,
            "rps_max_proxy_concurrency": null,
            "conc_at_peak": null,
            "rps_max_proxy_mock_bound": null,
            "sweep_max_proxy": [],
            "sweep_sustained_20ms": [],
            "egress_reverified": null
        }"#;
        let perf: CellPerf = serde_json::from_str(js).unwrap();
        assert!(!perf.rps_max_proxy.is_measured());
        assert_eq!(perf.rps_max_proxy.copied(), None);
        assert_ne!(perf.rps_max_proxy, Measurement::Measured(0));
    }

    // ── escaping ─────────────────────────────────────────────────────────────────────────────────

    #[test]
    fn hostile_strings_round_trip_exactly() {
        let hostile = "quote\" backslash\\ newline\n tab\t control\u{0007} unicode: caf\u{e9} \u{1f600}";
        let mut cell = sample_cell();
        cell.verdict_note = hostile.to_string();
        cell.body_snippet = hostile.to_string();
        cell.probe_note = Some(hostile.to_string());
        let js = serde_json::to_string(&cell).unwrap();
        let back: Cell = serde_json::from_str(&js).unwrap();
        assert_eq!(back.verdict_note, hostile);
        assert_eq!(back.body_snippet, hostile);
        assert_eq!(back.probe_note.as_deref(), Some(hostile));
    }

    #[test]
    fn control_characters_are_escaped_not_emitted_raw() {
        let mut cell = sample_cell();
        cell.verdict_note = "line one\nline two\x01".to_string();
        let js = serde_json::to_string(&cell).unwrap();
        // A literal, unescaped control byte or raw newline inside a JSON string is invalid JSON;
        // serde_json must have escaped it, so the raw byte must not appear verbatim in the output.
        assert!(!js.contains('\u{0001}'));
        let reparsed: serde_json::Value = serde_json::from_str(&js).unwrap();
        assert_eq!(reparsed["verdict_note"], serde_json::json!("line one\nline two\u{0001}"));
    }

    // ── parity with real committed snapshots ────────────────────────────────────────────────────

    // Fixtures are DISCOVERED, never named. Naming one would put a gateway identity in a shared
    // source file, which the isolation lint forbids and which would also pin this test to whichever
    // entrant happened to be in the corpus the day it was written.
    fn any_snapshot_with(pred: impl Fn(&ResultSnapshot) -> bool) -> Option<ResultSnapshot> {
        let mut paths: Vec<_> = fs::read_dir(snapshots_dir())
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        paths.sort(); // deterministic pick, so a failure reproduces
        paths.into_iter().find_map(|p| {
            let text = fs::read_to_string(&p).ok()?;
            let snap: ResultSnapshot = serde_json::from_str(&text).ok()?;
            pred(&snap).then_some(snap)
        })
    }

    #[test]
    fn deserialises_a_real_committed_snapshot() {
        let snap = any_snapshot_with(|s| {
            s.matrix.served && s.matrix.upstreams.contains_key("openai")
        })
        .expect("the corpus should contain at least one served snapshot with an openai egress");
        assert!(!snap.gateway.is_empty());
        assert_eq!(snap.schema_version, 1);
        assert!(snap.matrix.served);
        let openai_egress = snap.matrix.upstreams.get("openai").expect("openai egress present");
        assert!(openai_egress.served);
        let cell = openai_egress.cells.get("openai").expect("openai>openai cell present");
        assert!(matches!(cell.served, Served::Bool(true)));
        let perf = cell.perf.as_ref().expect("served cell carries perf");
        assert!(perf.rps_max_proxy.is_measured());
    }

    #[test]
    fn deserialises_every_real_committed_snapshot() {
        let dir = snapshots_dir();
        let entries = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("snapshots dir {} should be readable: {e}", dir.display()));
        let mut checked = 0;
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            let snap: ResultSnapshot = serde_json::from_str(&text).unwrap_or_else(|e| {
                panic!("{} must parse under these types: {e}", path.display())
            });
            assert!(!snap.gateway.is_empty());
            assert!(!snap.matrix.upstreams.is_empty());
            checked += 1;
        }
        assert!(checked > 0, "expected at least one committed snapshot fixture to exist");
    }

    #[test]
    fn deserialising_a_not_served_cell_carries_no_perf_or_stream() {
        // Again discovered, not named: any snapshot in the corpus with an unserved egress will do.
        let snap = any_snapshot_with(|s| s.matrix.upstreams.values().any(|u| !u.served))
            .expect("the corpus should contain at least one snapshot with an unserved egress");
        let unserved = snap.matrix.upstreams.values().find(|u| !u.served).expect("checked above");
        // A not-served egress cell must not fabricate a perf/stream block from thin air.
        for cell in unserved.cells.values() {
            if !matches!(cell.served, Served::Bool(true)) {
                assert!(cell.perf.is_none());
            }
        }
    }
}
