// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The shape of the artifact this engine publishes, serialised by serde rather than hand-rolled string
// concatenation. Every published metric is a `Measurement<T>` (see measurement.rs), so an unmeasured
// cell reads as `null` and never as a 0 a chart would draw; structural fields (`served`, `status`,
// dialect names) stay plain strings/bools/enums since that discipline is only for numbers a reader
// could mistake for a result.
//
// Built from matrix/run.sh's `emit_cell` plus real committed snapshots under results/snapshots/.
// Fields confirmed against a real snapshot are typed precisely; fields the shell defines but this repo
// has never populated (per-cell memory windows, a fully-measured streaming block, box-qualification
// stage bodies) stay opt-in permissive (`serde_json::Value` or `Option`). `matrix_version: 2` is the
// schema this module targets; older snapshots predate several fields, hence the `Option` +
// `#[serde(default)]` outside the core measurement grid.

use crate::measurement::{Absent, AbsentEntry, Measurement};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// `#[serde(default)]` needs a concrete `Default` impl, and `Measurement<T>` deliberately has none (a
/// silent default would be one step from the `value_or_zero` this module exists to forbid). A field
/// the wire omits entirely fills in as `NotMeasured`, the same as a `null` does, never as a zero.
fn measurement_default<T>() -> Measurement<T> {
    Measurement::absent(Absent::NotMeasured)
}

/// A handful of list fields (chiefly `rss_series`) come back as JSON `null` rather than `[]` when the
/// shell had nothing to put there. `Vec<T>` has no `Deserialize` for `null`, so this folds `null` to
/// the empty vec instead of failing the whole snapshot's parse.
fn null_as_empty_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

/// The top-level artifact written to `results/snapshots/result_<gateway>_<ts>.json`.
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultSnapshot {
    pub schema_version: u32,
    /// What each published metric means, keyed by metric name. Formatted from the engine's own
    /// constants rather than hand-written, so the definition cannot drift from the code the way a
    /// hand-written one once did (docs said "p99 < 1s" while the engine enforced 20ms).
    ///
    /// Top-level, not per cell: a definition doesn't vary by cell, and repeating it 36 times would
    /// create 36 places for it to drift.
    #[serde(default)]
    pub definitions: std::collections::BTreeMap<String, String>,
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
    /// The measurement instrument's own provenance (rig binaries + box qualification). Best-effort:
    /// a consumer must read "no rig block" as "not recorded", never as "not qualified".
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
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Whether/how a cell was served. Not a `Measurement`: a verdict label, not a number a chart could
/// plot. Non-`true` values are deliberately readable strings, exactly as the shell wrote them:
/// `false`, `"not_configured"`, `"not_configurable"`, `"unprobed_auth"`, `"not_verified"`, `"untestable"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Served {
    Bool(bool),
    Status(String),
}

/// The conservative default: a cell nobody has probed has not been shown to be served. Defaulting to
/// `true` would let a struct built with `..Default::default()` claim a capability by omission.
impl Default for Served {
    fn default() -> Self {
        Served::Status("not_probed".into())
    }
}

/// One probed (ingress, egress) pairing.
///
/// Serialised by hand (see the `Serialize` impl below `CellMemory`), not derived: the wire form adds
/// a computed `absences` map (metric name -> why it is absent), gathered from `perf`/`stream`/`memory`
/// at serialisation time so it's covered from one place rather than at every `Cell`-building call site.
#[derive(Default, Debug, Clone, PartialEq, Deserialize)]
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
    /// Seconds per metric group for this cell, keyed by the group's own name. Lets a slow run be
    /// diagnosed from committed JSON (which group got slower and why) rather than re-run with a
    /// stopwatch.
    #[serde(default)]
    pub timings_s: Option<std::collections::BTreeMap<String, f64>>,
}

/// One reading of the sweep, at one declared tail-latency bound: the most throughput the gateway
/// carried while 99% of requests finished under `p99_bound_us` and it failed none it accepted.
///
/// The frontier of these replaces `rps_max_proxy` and `rps_sustained_20ms`, which collapsed the same
/// sweep to two scalars at a chosen ceiling and let them invert against each other (see `frontier.rs`).
///
/// Every field is evidence for the one above it: `rps` is the claim, `concurrency` is where it was
/// observed, `p99_us` is the tail it actually came with (never the bound itself), and
/// `first_disqualified_conc` is what makes "the most under this bound" checkable rather than asserted.
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrontierReading {
    /// The bound, in microseconds. `None` is the failure-only reading: no latency constraint at all,
    /// answering "how much can it carry before it starts failing requests".
    #[serde(default)]
    pub p99_bound_us: Option<i64>,
    /// Published as a float (fractional below 1/s) so a cell serving 0.25 req/s doesn't report `0`.
    #[serde(default = "measurement_default")]
    pub rps: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub concurrency: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub first_disqualified_conc: Measurement<i64>,
    /// True when the sweep ran out of range while still qualifying, so `rps` is a lower bound rather
    /// than a ceiling. The retired search instead discarded the rate as `SearchExhausted`; the rate is
    /// right either way, only the label changes.
    #[serde(default)]
    pub lower_bound: bool,
}

/// One rung of a concurrency sweep. `rps` / `p99_us` / `fail` are `Measurement` on principle (a rung
/// the harness could not sample would otherwise have nowhere honest to put that fact), even though
/// every rung the shell engine has ever published carried real values for all three.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SweepPoint {
    pub conc: i64,
    /// Successful completions in this window. Published so a reader can apply the engine's own
    /// "served cleanly" rule (`frontier::Rung::served_cleanly`: `ok > 0 && fail == 0`) exactly, rather
    /// than approximating it from `rps > 0` - which once produced a false alarm on a rung that
    /// completed one request over four seconds (`ok >= 1`, clean) but rounded `rps` down to 0.
    #[serde(default = "measurement_default")]
    pub ok: Measurement<i64>,
    /// See the frontier reading's `rps`: fractional below 1/s for the same reason.
    #[serde(default = "measurement_default")]
    pub rps: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub fail: Measurement<i64>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellPerf {
    #[serde(default = "measurement_default")]
    pub added_latency_p50_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub added_latency_p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub gateway_c1_p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub direct_c1_p99_us: Measurement<i64>,
    /// What a request cost, in microseconds of gateway CPU. Unlike throughput, this still
    /// distinguishes two gateways at saturation: the one doing less work per request reads lower here,
    /// which is also the figure that maps to cost. Absent (never 0) on snapshots taken before this
    /// existed.
    #[serde(default = "measurement_default")]
    pub cpu_us_per_request: Measurement<f64>,
    /// The same fact the way an operator sizes a box: requests served per second of CPU burned.
    #[serde(default = "measurement_default")]
    pub rps_per_cpu_second: Measurement<f64>,
    /// The concurrency the two above were taken at. No single concurrency is sub-saturation across a
    /// field spanning 19 to 49,000 rps, so matched concurrency stands in for matched load, and a
    /// reader needs to see which it was.
    #[serde(default = "measurement_default")]
    pub cost_window_conc: Measurement<i64>,

    /// Requests the cost window actually completed, and the rate it carried. Needed to check
    /// `cpu_us_per_request` (CPU / this count) and to tell a genuinely cheap gateway from a window
    /// that simply carried less load than the sweep did at the same concurrency.
    #[serde(default = "measurement_default")]
    pub cost_window_ok: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub cost_window_rps: Measurement<f64>,
    /// Utilisation of the cores the gateway was pinned to, across the cost window (1.0 = fully busy).
    /// Distinguishes a real CPU-bound peak (near 1.0) from one bound by something else.
    #[serde(default = "measurement_default")]
    pub cost_core_utilisation: Measurement<f64>,
    #[serde(default = "measurement_default")]
    /// Threads in the gateway's tree during the cost window - the shape of its concurrency model
    /// (thread-per-connection vs. async), on evidence rather than a claim.
    pub cost_threads: Measurement<f64>,
    /// Involuntary context switches per request - the scheduler taking the CPU away, i.e. a saturated
    /// core from inside the process. The best single explainer of a tail.
    #[serde(default = "measurement_default")]
    pub cost_nonvol_ctxt_per_request: Measurement<f64>,
    /// Major faults during the cost window. Non-zero means the box was swapping, so the window timed
    /// the disk rather than the gateway; the cost figures above are re-flagged as a harness fault when
    /// this is not 0.
    #[serde(default = "measurement_default")]
    pub cost_majflt: Measurement<f64>,
    /// The frontier: one reading per declared tail-latency bound, ascending, with the failure-only
    /// reading last. Replaces `rps_max_proxy` / `rps_sustained_20ms`. Monotone non-decreasing in the
    /// bound by construction (see `frontier.rs`); `bench-audit.py` asserts it.
    #[serde(default)]
    pub frontier: Vec<FrontierReading>,
    #[serde(default)]
    pub sweep_max_proxy: Vec<SweepPoint>,
    #[serde(default)]
    pub egress_reverified: Option<bool>,
    #[serde(default)]
    pub reverify_note: Option<String>,
    #[serde(default)]
    pub c1_note: Option<String>,
}

/// Populates an absences map from a fixed list of `Measurement` fields on `$self`, keyed by
/// `stringify!`ing each field so the published key can never drift from the field it names (a hand-
/// typed string literal per field would be a second source of truth for the same name).
macro_rules! absences_of {
    ($self:expr, $($field:ident),+ $(,)?) => {{
        let mut out = BTreeMap::new();
        $( $self.$field.record_absence(stringify!($field), &mut out); )+
        out
    }};
}
// Callers needing keys the macro cannot reach (a Vec field's per-element absences, see
// `CellPerf::absences`) bind its result mutably and extend it, rather than the macro taking arbitrary
// key expressions - which would give up the stringify! guarantee that a key can't drift from its field.

impl CellPerf {
    /// Every absent metric on this block, keyed by its own field name. Empty when nothing is absent.
    pub fn absences(&self) -> BTreeMap<String, AbsentEntry> {
        let mut out = absences_of!(
            self,
            added_latency_p50_us,
            added_latency_p99_us,
            gateway_c1_p99_us,
            direct_c1_p99_us,
            // The cost fields: omitting them once published a bare null with no reason, which for
            // cost is worse than usual since the likely reasons (window had failures, box was
            // swapping) are refusals the reader must see.
            cpu_us_per_request,
            rps_per_cpu_second,
            cost_window_conc,
            cost_window_ok,
            cost_window_rps,
            cost_core_utilisation,
            cost_threads,
            cost_nonvol_ctxt_per_request,
            cost_majflt,
        );
        // Every frontier reading's own absences, since `Vec<FrontierReading>` is unreachable by
        // `absences_of!`. Keyed by the bound (`frontier.10ms.rps`) rather than array index, so looking
        // up a column doesn't require counting and inserting a bound later can't renumber later keys.
        for r in &self.frontier {
            let at = match r.p99_bound_us {
                Some(us) => format!("{}ms", us / 1000),
                None => "unbounded".to_string(),
            };
            r.rps
                .record_absence(&format!("frontier.{at}.rps"), &mut out);
            r.concurrency
                .record_absence(&format!("frontier.{at}.concurrency"), &mut out);
            r.p99_us
                .record_absence(&format!("frontier.{at}.p99_us"), &mut out);
            // `first_disqualified_conc` is deliberately not recorded here: its absence is the
            // positive finding `lower_bound: true` already states, not a hole to explain twice.
        }
        // Every rung's own absences, for the same reason as the frontier's above: `sweep_max_proxy`
        // is a Vec of `SweepPoint`s unreachable by `absences_of!`. Keyed by concurrency + window
        // ordinal (not array index) so repeated windows at one concurrency don't collide and inserting
        // a rung can't renumber existing keys.
        let mut seen_at: BTreeMap<i64, usize> = BTreeMap::new();
        for pt in &self.sweep_max_proxy {
            let n = seen_at.entry(pt.conc).or_insert(0);
            let at = format!("sweep.c{}.w{}", pt.conc, *n + 1);
            *n += 1;
            pt.ok.record_absence(&format!("{at}.ok"), &mut out);
            pt.rps.record_absence(&format!("{at}.rps"), &mut out);
            pt.p99_us.record_absence(&format!("{at}.p99_us"), &mut out);
            pt.fail.record_absence(&format!("{at}.fail"), &mut out);
        }
        out
    }
}

/// Whether/how a cell's streaming path was exercised. Like `Served`, this is a verdict label rather
/// than a number. Full vocabulary, since the producer (`suite::cell_stream`) can emit any `Absent`
/// token here, not just the three this doc once named:
///
/// - `true`: at least one gateway-vs-direct streaming comparison produced a number. `Cell::stream.reason`
///   then names the token of any comparison that did NOT produce one.
/// - any `Absent` token (`"not_measured"`, `"rig_limited"`, `"untestable"`, ...): probed, no comparison
///   produced a number; the token is that absence's own reason, so a rig limit is never published as a
///   claim about the gateway.
/// - `"not_probed"`: the streaming group did not run on this cell at all.
/// - `false`: never written by this engine (would wrongly assert the gateway does not stream). Kept
///   representable so older artifacts still parse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StreamServed {
    Bool(bool),
    Status(String),
}

/// Same conservative default as `Served`: nothing has been shown to stream until it was probed.
impl Default for StreamServed {
    fn default() -> Self {
        StreamServed::Status("not_probed".into())
    }
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellStream {
    pub stream_served: StreamServed,
    /// The machine-readable reason token behind a non-`true` `stream_served` (or a `true` one whose
    /// TTFT half is absent) - same vocabulary as `Cell::reason`. Prose lives in `stream_error` below,
    /// not here.
    #[serde(default)]
    pub reason: Option<String>,
    /// The operator-facing detail behind `reason`, when the absence carried one. Prose: nothing may
    /// branch on it.
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
    /// The most frames/sec the mock can emit at `streams_sustained` concurrency, and the fraction
    /// carried. Derived (exact arithmetic) from the mock's declared pacing, not measured - see
    /// `run::mock_frame_ceiling_fps`; ~1.0 means the gateway forwards every frame as it arrives.
    #[serde(default)]
    pub streams_sustained_mock_ceiling: Option<f64>,
    #[serde(default)]
    pub streams_sustained_headroom: Option<f64>,
    /// The sweep points behind `streams_sustained`, shaped like `SweepPoint` in run.sh's accumulator.
    /// No committed snapshot has populated it, so kept as opaque JSON rather than typed against an
    /// unconfirmed shape.
    #[serde(default)]
    pub sweep_streams: Vec<serde_json::Value>,
    #[serde(default)]
    pub stream_c1_note: Option<String>,
    /// How many TTFT probes survived per leg, so a reader can weigh `added_ttft_p50_us` /
    /// `added_ttft_p99_us` (a percentile over 3 samples vs. 100 reads identically without this).
    #[serde(default = "measurement_default")]
    pub ttft_gw_samples: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub ttft_direct_samples: Measurement<i64>,
}

impl CellStream {
    pub fn absences(&self) -> BTreeMap<String, AbsentEntry> {
        let mut out = absences_of!(
            self,
            added_ttft_p50_us,
            added_ttft_p99_us,
            added_gap_p50_us,
            added_gap_p99_us,
            ttft_gw_samples,
            ttft_direct_samples,
            streams_sustained,
            streams_sustained_fps,
        );
        // The two derived fields (`streams_sustained_mock_ceiling`, `_headroom`) are `Option<f64>`,
        // not `Measurement`, so `absences_of!` can't cover them; they inherit the parent's reason
        // since they're derived from `streams_sustained`. The check tests the fields themselves
        // (`is_none()`), not merely whether the parent is absent: `suite` can produce both as `None`
        // while `streams_sustained` is measured (e.g. a bisect landing on conc == 0), which the
        // parent-only guard used to miss, leaving a bare null on a served cell.
        for key in [
            "streams_sustained_mock_ceiling",
            "streams_sustained_headroom",
        ] {
            let missing = match key {
                "streams_sustained_mock_ceiling" => self.streams_sustained_mock_ceiling.is_none(),
                _ => self.streams_sustained_headroom.is_none(),
            };
            if !missing {
                continue;
            }
            // Inherit the parent's reason when it has one - these ARE derived from it, so its reason
            // is a statement of fact rather than an invented one. When the parent is measured, the
            // honest reason is that the rig could not state the ceiling this fraction is taken of.
            let entry = match &self.streams_sustained {
                Measurement::Absent { reason, detail } => AbsentEntry {
                    reason: reason.clone(),
                    detail: detail.clone(),
                },
                _ => AbsentEntry {
                    reason: Absent::NotMeasured,
                    detail: Some(
                        "the sustained figure was measured but the rig could not derive the mock's \
                         frame ceiling for this cell, so there is nothing to state this as a fraction of"
                            .to_string(),
                    ),
                },
            };
            out.entry(key.to_string()).or_insert(entry);
        }
        out
    }
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

/// The per-cell memory window (`CELL_MEM_JSON` in run.sh): its own cold-started, plateau-terminated
/// window for one served cell - what THIS cell does run cold, not what the peak cell does after the
/// whole grid ran (that's `MatrixMemory`). No committed snapshot has populated this yet: typed from
/// the shell source, kept permissive (`Option` throughout).
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Whether the window settled. A `Measurement`, not `Option<bool>`: "could not tell" is an
    /// absence with a reason like any other, and as a plain `Option` it published a bare null that
    /// `absences()` (which walks `Measurement` fields only) could not see or explain. Wire form
    /// unchanged.
    #[serde(default = "measurement_default")]
    pub plateaued: Measurement<bool>,
    #[serde(default = "measurement_default")]
    pub time_to_plateau_s: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub growth_rate_mib_per_min: Measurement<f64>,
    /// How long load was applied. `Measurement` for the same reason `plateaued` is.
    #[serde(default = "measurement_default")]
    pub load_s: Measurement<i64>,
    /// How the window failed to settle, when it did: 1 climbing, 0 oscillating, -1 falling, absent
    /// when it settled. Distinguishes an unbounded climb from a GC-driven oscillation - collapsed to
    /// one word, the board would flag the oscillating gateway as a leak it doesn't have.
    #[serde(default = "measurement_default")]
    pub shape: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub idle_shape: Measurement<f64>,
    /// The idle-leak verdict and its rate - `metric.rs` calls a climb at idle the most damning memory
    /// result there is, so a missing key here (vs. a null) must not read as "holding flat".
    #[serde(default = "measurement_default")]
    pub idle_static: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub idle_growth_rate_mib_per_min: Measurement<f64>,
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub rss_series: Vec<RssSample>,
    /// The idle window's own readings, kept apart from `rss_series` since the two answer different
    /// questions (cost at rest vs. cost under work). Empty means "not recorded", never a flat line.
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub idle_rss_series: Vec<RssSample>,
    #[serde(default)]
    pub idle_window_s: Option<i64>,
    #[serde(default)]
    pub recovery_window_s: Option<i64>,
}

impl CellMemory {
    pub fn absences(&self) -> BTreeMap<String, AbsentEntry> {
        absences_of!(
            self,
            idle_rss_mib,
            steady_state_rss_mib,
            recovered_rss_mib,
            peak_rss_mib,
            peak_rss_hwm_mib,
            time_to_plateau_s,
            growth_rate_mib_per_min,
            // `plateaued`/`load_s` used to publish a bare null on a served cell when they were plain
            // `Option`s; now covered here like every other Measurement.
            plateaued,
            load_s,
            shape,
            idle_shape,
            idle_static,
            idle_growth_rate_mib_per_min,
        )
    }
}

/// The wire form of `Cell`: the normal fields verbatim, plus a computed `absences` map merging
/// `perf`/`stream`/`memory`'s own absences under a `"perf."`/`"stream."`/`"memory."` prefix so a
/// reader sees exactly which metric was absent and why, without guessing which sub-block it lives in.
impl Serialize for Cell {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut absences: BTreeMap<String, AbsentEntry> = BTreeMap::new();
        if let Some(perf) = &self.perf {
            absences.extend(
                perf.absences()
                    .into_iter()
                    .map(|(k, v)| (format!("perf.{k}"), v)),
            );
        }
        if let Some(stream) = &self.stream {
            absences.extend(
                stream
                    .absences()
                    .into_iter()
                    .map(|(k, v)| (format!("stream.{k}"), v)),
            );
        }
        if let Some(memory) = &self.memory {
            absences.extend(
                memory
                    .absences()
                    .into_iter()
                    .map(|(k, v)| (format!("memory.{k}"), v)),
            );
        }

        let mut st = s.serialize_struct("Cell", 13)?;
        st.serialize_field("served", &self.served)?;
        st.serialize_field("reason", &self.reason)?;
        st.serialize_field("status", &self.status)?;
        st.serialize_field("path", &self.path)?;
        st.serialize_field("verdict_note", &self.verdict_note)?;
        st.serialize_field("body_snippet", &self.body_snippet)?;
        st.serialize_field("probe_note", &self.probe_note)?;
        st.serialize_field("perf_dropped", &self.perf_dropped)?;
        st.serialize_field("perf", &self.perf)?;
        st.serialize_field("stream", &self.stream)?;
        st.serialize_field("memory", &self.memory)?;
        // The suite fills this deliberately ("Cost belongs in the artifact") and this hand-written
        // list silently dropped it: the one field the derive would have carried for free.
        st.serialize_field("timings_s", &self.timings_s)?;
        st.serialize_field("absences", &absences)?;
        st.end()
    }
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
}

#[cfg(test)]
mod tests {
    /// Structural guard, not a list to maintain: serialises a perf record whose scalars are all
    /// absent, then asserts every key that serialised to `null` has a reason in the sibling
    /// `absences()` map. A field added to the struct but forgotten in `absences_of!` publishes a bare
    /// null with no reason - this has happened twice (the frontier Vec, then the cost fields), so it's
    /// a test rather than a comment now.
    #[test]
    fn every_absent_perf_scalar_carries_a_reason() {
        use crate::measurement::{Absent, Measurement};
        // All-absent, each with a reason, so anything that loses one is visible.
        let mut p = CellPerf::default();
        macro_rules! blank {
            ($($f:ident),* $(,)?) => {$(
                p.$f = Measurement::absent_because(Absent::NotMeasured, concat!("test: ", stringify!($f)));
            )*};
        }
        blank!(
            added_latency_p50_us,
            added_latency_p99_us,
            gateway_c1_p99_us,
            direct_c1_p99_us,
            cpu_us_per_request,
            rps_per_cpu_second,
            cost_window_conc,
            cost_window_ok,
            cost_window_rps,
            cost_core_utilisation,
            cost_threads,
            cost_nonvol_ctxt_per_request,
            cost_majflt,
        );
        // Not measurements (`c1_note`/`reverify_note` are prose, `egress_reverified` a verdict flag),
        // so given values rather than exempted by name - keeps the test about measurements only.
        p.c1_note = Some("test".into());
        p.reverify_note = Some("test".into());
        p.egress_reverified = Some(true);

        let reasons = p.absences();
        let json = serde_json::to_value(&p).expect("serialises");
        let obj = json.as_object().expect("an object");
        let mut orphans: Vec<&str> = Vec::new();
        for (k, v) in obj {
            // Only scalars that serialised to null are measurements that went absent; arrays and
            // strings are evidence or prose and are not this map's business.
            if v.is_null() && !reasons.contains_key(k.as_str()) {
                orphans.push(k.as_str());
            }
        }
        assert!(
            orphans.is_empty(),
            "these absent fields published a null with no reason: {orphans:?} - add them to absences_of! in CellPerf::absences"
        );
    }

    fn sample_perf() -> CellPerf {
        CellPerf {
            added_latency_p50_us: Measurement::Measured(40_939),
            added_latency_p99_us: Measurement::Measured(40_945),
            gateway_c1_p99_us: Measurement::Measured(41_026),
            direct_c1_p99_us: Measurement::Measured(81),
            // A value the fixture can round-trip, so a field left at its default doesn't pass vacuously.
            cpu_us_per_request: Measurement::Measured(37.5),
            rps_per_cpu_second: Measurement::Measured(26_666.0),
            cost_window_conc: Measurement::Measured(8),
            cost_window_ok: Measurement::Measured(2048.0),
            cost_window_rps: Measurement::Measured(341.0),
            cost_core_utilisation: Measurement::Measured(0.97),
            cost_threads: Measurement::Measured(9.0),
            cost_nonvol_ctxt_per_request: Measurement::Measured(0.25),
            cost_majflt: Measurement::Measured(0.0),
            // The throughput answer is the frontier now; `rps_max_proxy`/`rps_sustained_20ms` and
            // their concurrency twins are gone from the artifact.
            frontier: Vec::new(),
            sweep_max_proxy: vec![SweepPoint {
                conc: 256,
                ok: Measurement::Measured(1_000),
                rps: Measurement::Measured(6_209.0),
                p99_us: Measurement::Measured(43_969),
                fail: Measurement::Measured(0),
            }],
            egress_reverified: Some(true),
            reverify_note: None,
            c1_note: None,
        }
    }

    // `frontier.rs` proves ordering holds over rungs; this proves the published shape keeps it, so a
    // serialization bug can't reorder or drop readings while every individual number stays correct.
    #[test]
    fn a_serialized_frontier_keeps_its_order_and_its_monotonicity() {
        use super::*;
        // apisix anthropic>anthropic, 2026-07-29: the real curve.
        let rows = [
            (Some(1_000i64), 7_015i64),
            (Some(5_000), 15_438),
            (Some(10_000), 18_943),
            (Some(50_000), 19_284),
            (Some(100_000), 19_284),
            (None, 19_284),
        ];
        let frontier: Vec<FrontierReading> = rows
            .iter()
            .map(|(b, rps)| FrontierReading {
                p99_bound_us: *b,
                rps: Measurement::Measured(*rps as f64),
                concurrency: Measurement::Measured(256),
                p99_us: Measurement::Measured(4_000),
                first_disqualified_conc: Measurement::Measured(1024),
                lower_bound: false,
            })
            .collect();
        let text = serde_json::to_string(&frontier).expect("serialize");
        let back: Vec<FrontierReading> = serde_json::from_str(&text).expect("round trip");
        assert_eq!(back.len(), 6);
        // The unbounded reading is LAST and its bound serializes as null, not as a missing key or a
        // sentinel number - "no latency bound" is a distinct state from "some bound we forgot".
        assert_eq!(back[5].p99_bound_us, None);
        assert!(text.contains("\"p99_bound_us\":null"), "{text}");
        let bounds: Vec<Option<i64>> = back.iter().map(|r| r.p99_bound_us).collect();
        assert_eq!(
            bounds,
            vec![
                Some(1_000),
                Some(5_000),
                Some(10_000),
                Some(50_000),
                Some(100_000),
                None
            ],
            "bounds must stay ascending with the unbounded reading last"
        );
        let rates: Vec<f64> = back.iter().map(|r| r.rps.copied().unwrap()).collect();
        for w in rates.windows(2) {
            assert!(
                w[1] >= w[0],
                "the published sequence must not invert: {rates:?}"
            );
        }
    }

    // A reading that ran out of range publishes its rate AND says the rate is a floor. The retired
    // search published null here and threw the measurement away for failing to prove maximality.
    #[test]
    fn a_lower_bound_reading_publishes_its_rate_and_declares_itself_a_floor() {
        use super::*;
        let r = FrontierReading {
            p99_bound_us: Some(10_000),
            rps: Measurement::Measured(19_000.0),
            concurrency: Measurement::Measured(16_384),
            p99_us: Measurement::Measured(3_000),
            first_disqualified_conc: Measurement::absent(Absent::SearchExhausted),
            lower_bound: true,
        };
        let back: FrontierReading =
            serde_json::from_str(&serde_json::to_string(&r).expect("ser")).expect("de");
        assert_eq!(
            back.rps.copied(),
            Some(19_000.0),
            "the rate is real and is published"
        );
        assert!(
            back.lower_bound,
            "and it is labelled a floor rather than a ceiling"
        );
        // The reason doesn't survive the envelope - that's convention, not a defect: an absent
        // `Measurement` serializes as a bare null and its reason lives in the cell's sibling
        // `absences` map instead (tested next).
        assert_eq!(back.first_disqualified_conc.copied(), None);
    }

    // The absence map carries every reading's reason, keyed by its bound.
    #[test]
    fn an_absent_frontier_reading_publishes_its_reason_in_the_cells_absence_map() {
        let mut perf = sample_perf();
        perf.frontier = vec![
            FrontierReading {
                p99_bound_us: Some(1_000),
                rps: Measurement::absent_because(
                    Absent::BelowResolution,
                    "every cleanly-served rung had a tail at or above 1ms",
                ),
                concurrency: Measurement::absent(Absent::BelowResolution),
                p99_us: Measurement::absent(Absent::BelowResolution),
                first_disqualified_conc: Measurement::absent(Absent::BelowResolution),
                lower_bound: false,
            },
            FrontierReading {
                p99_bound_us: None,
                rps: Measurement::Measured(19_284.0),
                concurrency: Measurement::Measured(1024),
                p99_us: Measurement::Measured(40_000),
                first_disqualified_conc: Measurement::Measured(2048),
                lower_bound: false,
            },
        ];
        let abs = perf.absences();
        let e = abs
            .get("frontier.1ms.rps")
            .expect("keyed by its BOUND, so a reader need not count columns to find it");
        assert_eq!(e.reason, Absent::BelowResolution);
        assert!(e.detail.as_deref().unwrap_or_default().contains("1ms"));
        assert!(abs.contains_key("frontier.1ms.concurrency"));
        assert!(abs.contains_key("frontier.1ms.p99_us"));
        // The reading that WAS taken contributes no keys at all.
        assert!(!abs.keys().any(|k| k.starts_with("frontier.unbounded")));
    }

    use super::*;
    use crate::measurement::Absent;

    // The published memory field names are a wire contract with `site/gen-data.mjs`, `site/seal.mjs`
    // and `site/check-consistency.mjs`, which read these keys by literal name and silently see
    // `undefined` (not an error) on rename. Listed literally, not derived from the struct, so a rename
    // here can't make this test agree with itself.
    #[test]
    fn the_memory_field_names_the_site_reads_are_pinned_to_the_wire() {
        // Every field either carries a serde default or is filled here, so this deserialize also
        // proves the defaults still cover the shape.
        let matrix_memory: MatrixMemory = serde_json::from_str(r#"{"served":true}"#).unwrap();
        let v = serde_json::to_value(&matrix_memory).unwrap();
        for key in [
            "idle_rss_mib",
            "peak_rss_mib",
            "peak_rss_hwm_mib",
            "post_load_rss_mib",
            "recovered_rss_mib",
        ] {
            assert!(
                v.get(key).is_some(),
                "matrix memory must publish {key}: site/ reads it by this exact name"
            );
        }

        let cell_memory = CellMemory::default();
        let v = serde_json::to_value(&cell_memory).unwrap();
        for key in [
            "idle_rss_mib",
            "peak_rss_mib",
            "peak_rss_hwm_mib",
            "steady_state_rss_mib",
            "recovered_rss_mib",
            "time_to_plateau_s",
            "growth_rate_mib_per_min",
            // Both changed from plain `Option` to `Measurement` so their absence can carry a reason.
            // The wire name and the wire form (bare value or null) are unchanged, and this is where
            // that is pinned.
            "plateaued",
            "load_s",
        ] {
            assert!(
                v.get(key).is_some(),
                "cell memory must publish {key}: site/ reads it by this exact name"
            );
        }
        assert_eq!(
            v["plateaued"],
            serde_json::Value::Null,
            "an unjudged plateau is a null on the wire, not a false"
        );
        assert_eq!(v["load_s"], serde_json::Value::Null);
        let back: CellMemory =
            serde_json::from_str(r#"{"served":true,"plateaued":true,"load_s":90}"#)
                .expect("the published form must parse back");
        assert_eq!(back.plateaued.copied(), Some(true));
        assert_eq!(back.load_s.copied(), Some(90));
    }

    // Every null on a served cell carries a reason: `plateaued`/`load_s` used to be plain `Option`s,
    // invisible to `absences_of!`, so an unjudged plateau published a bare null with no explanation.
    #[test]
    fn a_memory_window_that_could_not_judge_the_plateau_publishes_why_not_a_bare_null() {
        let cell = Cell {
            served: Served::Bool(true),
            memory: Some(CellMemory {
                served: true,
                plateaued: Measurement::absent_because(
                    Absent::NotMeasured,
                    "too few readings fell inside the settle window to judge whether memory moved",
                ),
                load_s: Measurement::absent(Absent::HarnessError),
                ..Default::default()
            }),
            ..Default::default()
        };
        let v: serde_json::Value = serde_json::to_value(&cell)
            .expect("a cell must serialise")
            .clone();

        assert_eq!(
            v["memory"]["plateaued"],
            serde_json::Value::Null,
            "the value slot stays a bare null for existing consumers"
        );
        assert_eq!(
            v["absences"]["memory.plateaued"]["reason"], "not_measured",
            "the plateau verdict's absence must be published with its reason: got {v}"
        );
        assert!(
            v["absences"]["memory.plateaued"]["detail"]
                .as_str()
                .unwrap_or_default()
                .contains("settle window"),
            "and with the window's own evidence: got {v}"
        );
        assert_eq!(
            v["absences"]["memory.load_s"]["reason"], "harness_error",
            "load_s must be coverable too: got {v}"
        );
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
                // Token in `reason`, prose in `stream_error` - keeping them separate fields.
                reason: Some("untestable".to_string()),
                stream_error: Some("did not rebind the mock port".to_string()),
                // NotMeasured, not Untestable: a null on the wire always deserialises back to
                // NotMeasured, so a round-trippable fixture must start from the reason that survives.
                added_ttft_p50_us: Measurement::absent(Absent::NotMeasured),
                added_ttft_p99_us: Measurement::absent(Absent::NotMeasured),
                ttft_gw_samples: Measurement::absent(Absent::NotMeasured),
                ttft_direct_samples: Measurement::absent(Absent::NotMeasured),
                added_gap_p50_us: Measurement::absent(Absent::NotMeasured),
                added_gap_p99_us: Measurement::absent(Absent::NotMeasured),
                streams_sustained: Measurement::absent(Absent::NotMeasured),
                streams_sustained_fps: Measurement::absent(Absent::NotMeasured),
                streams_sustained_mock_ceiling: None,
                streams_sustained_headroom: None,
                sweep_streams: vec![],
                stream_c1_note: None,
            }),
            memory: None,
            timings_s: None,
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
            definitions: Default::default(),
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
            config: ConfigFiles {
                files: HashMap::new(),
            },
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

    // Regression pin: `Cell`'s hand-written `Serialize` lists fields one by one, so the compiler
    // can't catch a forgotten `st.serialize_field` call the way it catches a forgotten struct
    // literal field. `timings_s` was added to the struct and dropped by exactly this omission.
    #[test]
    fn timings_s_reaches_the_wire() {
        let mut cell = sample_cell();
        let mut timings = std::collections::BTreeMap::new();
        timings.insert("load".to_string(), 12.5);
        cell.timings_s = Some(timings);

        let v: serde_json::Value = serde_json::to_value(&cell).unwrap();
        assert_eq!(
            v.get("timings_s"),
            Some(&serde_json::json!({"load": 12.5})),
            "timings_s must be on the wire: got {v}"
        );

        let back: Cell = serde_json::from_str(&serde_json::to_string(&cell).unwrap()).unwrap();
        assert_eq!(back.timings_s, cell.timings_s);
    }

    #[test]
    fn served_status_variant_round_trips() {
        for s in [
            Served::Bool(true),
            Served::Bool(false),
            Served::Status("untestable".to_string()),
        ] {
            let js = serde_json::to_string(&s).unwrap();
            let back: Served = serde_json::from_str(&js).unwrap();
            assert_eq!(s, back);
        }
    }

    // ── golden shape ─────────────────────────────────────────────────────────────────────────────

    // ── absence ──────────────────────────────────────────────────────────────────────────────────

    #[test]
    fn absent_stream_metric_serialises_as_null() {
        let cell = sample_cell();
        let stream = cell.stream.unwrap();
        let js = serde_json::to_string(&stream).unwrap();
        let v: serde_json::Value = serde_json::from_str(&js).unwrap();
        assert_eq!(v["added_ttft_p99_us"], serde_json::Value::Null);
    }

    // `CellStream.reason` must stay a stable token (like `Cell.reason`), with prose in `stream_error`
    // - not the other way around, which made `reason` mean a token in one block and a sentence in
    // the other.
    #[test]
    fn a_stream_block_publishes_a_reason_token_with_its_prose_in_its_own_field() {
        let cell = sample_cell();
        let stream = cell.stream.expect("the fixture carries a stream block");
        let v: serde_json::Value = serde_json::to_value(&stream).expect("must serialise");
        let reason = v["reason"].as_str().unwrap_or_default();
        assert!(
            [
                Absent::NotServed,
                Absent::NotMeasured,
                Absent::BelowResolution,
                Absent::RigLimited,
                Absent::SearchExhausted,
                Absent::Untestable,
                Absent::HarnessError,
            ]
            .iter()
            .any(|a| a.token() == reason),
            "stream.reason must be a token from the absence vocabulary, got {reason:?}"
        );
        assert_eq!(
            v["stream_error"], "did not rebind the mock port",
            "the prose belongs in stream_error, not in reason"
        );
    }

    // ── escaping ─────────────────────────────────────────────────────────────────────────────────

    #[test]
    fn hostile_strings_round_trip_exactly() {
        let hostile =
            "quote\" backslash\\ newline\n tab\t control\u{0007} unicode: caf\u{e9} \u{1f600}";
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
        assert_eq!(
            reparsed["verdict_note"],
            serde_json::json!("line one\nline two\u{0001}")
        );
    }

    // ── the shapes the published artifact must hold ─────────────────────────────────────────────

    // Asserted against a snapshot built here (a property of the types), not against committed
    // results/snapshots/ files, which a `git rm` could silently remove coverage for. Real-artifact
    // parsing is covered separately by engine/tests/end_to_end.rs.

    // A cell that was not served must not carry numbers - fabricating a perf block would publish a
    // capability the gateway never demonstrated.
    #[test]
    fn an_unserved_cell_carries_no_perf_across_the_wire() {
        let mut snap = sample_record();
        for up in snap.matrix.upstreams.values_mut() {
            up.served = false;
            for cell in up.cells.values_mut() {
                cell.served = Served::Bool(false);
                cell.perf = None;
                cell.stream = None;
            }
        }
        let js = serde_json::to_string(&snap).expect("a snapshot must serialise");
        let back: ResultSnapshot = serde_json::from_str(&js).expect("its own output must parse");
        for up in back.matrix.upstreams.values() {
            assert!(!up.served);
            for cell in up.cells.values() {
                assert!(cell.perf.is_none(), "an unserved cell must not carry perf");
                assert!(
                    cell.stream.is_none(),
                    "an unserved cell must not carry stream"
                );
            }
        }
    }
}

#[cfg(test)]
mod derived_stream_absence_tests {
    use super::*;

    fn served_stream() -> CellStream {
        CellStream {
            stream_served: StreamServed::Bool(true),
            streams_sustained: Measurement::Measured(0),
            streams_sustained_mock_ceiling: None,
            streams_sustained_headroom: None,
            ..Default::default()
        }
    }

    // A derived field is still a published null: the old guard only ran when the parent was absent,
    // so a bisect landing on conc == 0 (or a non-positive mock frame ceiling) published these as null
    // on a served cell with nothing in the absences map to explain them.
    #[test]
    fn a_measured_parent_does_not_excuse_its_derived_fields_from_carrying_a_reason() {
        let a = served_stream().absences();
        for k in [
            "streams_sustained_mock_ceiling",
            "streams_sustained_headroom",
        ] {
            let e = a
                .get(k)
                .unwrap_or_else(|| panic!("{k} published a bare null on a served cell"));
            assert!(
                e.detail.as_deref().unwrap_or("").contains("frame ceiling"),
                "{k}'s reason must say what could not be derived: {:?}",
                e.detail
            );
        }
    }

    /// When the parent IS absent they still inherit its reason, which is the behaviour that already
    /// worked and must not regress.
    #[test]
    fn an_absent_parent_still_lends_its_reason_to_the_fields_derived_from_it() {
        let s = CellStream {
            streams_sustained: Measurement::absent_because(
                Absent::SearchExhausted,
                "still passing at the top of the range".to_string(),
            ),
            ..served_stream()
        };
        let a = s.absences();
        for k in [
            "streams_sustained_mock_ceiling",
            "streams_sustained_headroom",
        ] {
            let e = a.get(k).expect("must carry a reason");
            assert_eq!(
                e.reason,
                Absent::SearchExhausted,
                "{k} must inherit the parent's reason"
            );
        }
    }

    /// And a fully derived pair publishes no absence at all.
    #[test]
    fn measured_derived_fields_need_no_absence_entry() {
        let s = CellStream {
            streams_sustained: Measurement::Measured(128),
            streams_sustained_mock_ceiling: Some(4000.0),
            streams_sustained_headroom: Some(0.9),
            ..served_stream()
        };
        let a = s.absences();
        assert!(!a.contains_key("streams_sustained_mock_ceiling"));
        assert!(!a.contains_key("streams_sustained_headroom"));
    }
}

#[cfg(test)]
mod sweep_rung_absence_tests {
    use super::*;

    fn rung(conc: i64, measured: bool) -> SweepPoint {
        // Generic: this fixture mixes Measurement<i64> (the counts) with Measurement<f64> (the rate),
        // and a closure would fix itself to whichever it saw first.
        fn gone<T>() -> Measurement<T> {
            Measurement::absent_because(Absent::NotMeasured, "no window was recorded".to_string())
        }
        SweepPoint {
            conc,
            ok: if measured {
                Measurement::Measured(10_000)
            } else {
                gone()
            },
            rps: if measured {
                Measurement::Measured(6209.0)
            } else {
                gone()
            },
            p99_us: if measured {
                Measurement::Measured(4200)
            } else {
                gone()
            },
            fail: if measured {
                Measurement::Measured(0)
            } else {
                gone()
            },
        }
    }

    // sweep_max_proxy is a Vec, unreachable by absences_of!, so a rung with no window used to
    // publish three bare nulls with no reason - inputs `frontier::Rung::served_cleanly` needs, so a
    // checker couldn't tell "no requests completed" from "no window was recorded".
    #[test]
    fn an_unmeasured_rung_states_a_reason_for_every_null_it_publishes() {
        let p = CellPerf {
            sweep_max_proxy: vec![rung(1024, false)],
            ..Default::default()
        };
        let a = p.absences();
        for f in ["ok", "rps", "p99_us", "fail"] {
            let k = format!("sweep.c1024.w1.{f}");
            assert!(
                a.contains_key(&k),
                "{k} published a bare null; keys were {:?}",
                a.keys()
            );
        }
    }

    /// Keyed by concurrency AND window ordinal, so repeated windows at one concurrency do not
    /// collide - three windows per rung is the normal shape, and a collision would silently drop two
    /// of every three reasons.
    #[test]
    fn repeated_windows_at_one_concurrency_get_distinct_keys() {
        let p = CellPerf {
            sweep_max_proxy: vec![rung(64, false), rung(64, false), rung(64, false)],
            ..Default::default()
        };
        let a = p.absences();
        for w in 1..=3 {
            assert!(
                a.contains_key(&format!("sweep.c64.w{w}.ok")),
                "window {w} lost its reason"
            );
        }
    }

    /// A fully measured rung needs no entry at all.
    #[test]
    fn a_measured_rung_publishes_no_absence() {
        let p = CellPerf {
            sweep_max_proxy: vec![rung(64, true)],
            ..Default::default()
        };
        assert!(p.absences().keys().all(|k| !k.starts_with("sweep.")));
    }
}
