// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE SHAPE OF THE ARTIFACT THIS ENGINE PUBLISHES.
//
// The shape, described once and serialised by serde rather than by hand-rolled string
// concatenation, so the compiler checks that braces balance and that a value is escaped before it
// lands between two quotes. Every published metric is a `Measurement<T>` (see measurement.rs), so an
// unmeasured cell reads as `null` on the wire and never as a 0 that a chart would draw. Structural
// fields (`served`, `status`, dialect names) are not measurements and stay as plain
// strings/bools/enums: the discipline applies to numbers a reader could mistake for a result, not to
// labels.
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

use crate::measurement::{Absent, AbsentEntry, Measurement};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

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
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
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
/// a computed `absences` map (metric name -> why it is absent) alongside the normal fields, gathered
/// from `perf`/`stream`/`memory` at serialisation time so every one of the dozens of `Measurement`
/// fields on this cell is covered from one place rather than needing a matching edit at each of the
/// several call sites across the engine that build a `Cell`.
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
    /// SECONDS PER METRIC GROUP for this cell, keyed by the group's own name.
    ///
    /// Cost belongs in the artifact for the same reason every verdict's reason does: a run that got
    /// slower is a question, and a wall-clock total cannot answer it. Thirteen minutes a cell might
    /// be the TTFT sample set, a stream ladder reaching a higher rung, or a gateway that slowed down,
    /// and those have nothing in common as responses. With this, "what would halving the TTFT samples
    /// save" is arithmetic over committed JSON rather than another run with a stopwatch.
    #[serde(default)]
    pub timings_s: Option<std::collections::BTreeMap<String, f64>>,
}

/// ONE READING OF THE SWEEP, at one declared tail-latency bound: the most throughput the gateway
/// carried while 99% of requests finished under `p99_bound_us` and it failed none it accepted.
///
/// The frontier of these replaces `rps_max_proxy` and `rps_sustained_20ms`, which were the same sweep
/// collapsed to two scalars by a chosen ceiling. `frontier.rs`'s module note has the full reasoning;
/// the short version is that throughput and tail latency rise together, so "the throughput" is a point
/// on a curve, and picking the point for the reader both hid the tradeoff and let the two scalars
/// invert against each other.
///
/// EVERY FIELD IS EVIDENCE FOR THE ONE ABOVE IT. `rps` is the claim; `concurrency` is where it was
/// observed; `p99_us` is the tail it ACTUALLY came with (never the bound - 4ms under a 100ms bound is
/// not the same finding as 99ms); `first_disqualified_conc` is the lowest concurrency above it that
/// stopped qualifying, which is what makes "this is the most under this bound" checkable instead of
/// asserted.
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrontierReading {
    /// The bound, in microseconds. `None` is the failure-only reading: no latency constraint at all,
    /// answering "how much can it carry before it starts failing requests".
    #[serde(default)]
    pub p99_bound_us: Option<i64>,
    #[serde(default = "measurement_default")]
    pub rps: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub concurrency: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub first_disqualified_conc: Measurement<i64>,
    /// Did the sweep RUN OUT OF RANGE while still qualifying? Then `rps` is a lower bound, not a
    /// ceiling, and every surface must say so. The retired search published `SearchExhausted` - a null -
    /// for this state, discarding a real measured rate because it could not prove maximality. The rate
    /// is right either way; only the word for it changes.
    #[serde(default)]
    pub lower_bound: bool,
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
    /// THE FRONTIER: one reading per declared tail-latency bound, ascending, with the failure-only
    /// reading last. The published throughput answer, replacing `rps_max_proxy` /
    /// `rps_sustained_20ms`.
    ///
    /// Monotone non-decreasing in the bound BY CONSTRUCTION (see `frontier.rs`), so a reader can check
    /// the sequence by eye and `bench-audit.py` asserts it.
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
// NOTE: callers that need to add keys the macro cannot reach (a Vec field's per-element absences, see
// `CellPerf::absences`) bind its result mutably and extend it, rather than the macro growing a second
// shape. The macro's whole value is that every key it writes is `stringify!`d from the field it names
// and so cannot drift from it; a variant taking arbitrary key expressions would give that up.

impl CellPerf {
    /// Every absent metric on this block, keyed by its own field name. Empty when nothing is absent.
    pub fn absences(&self) -> BTreeMap<String, AbsentEntry> {
        let mut out = absences_of!(
            self,
            added_latency_p50_us,
            added_latency_p99_us,
            gateway_c1_p99_us,
            direct_c1_p99_us,
        );
        // AND EVERY FRONTIER READING'S OWN ABSENCES, keyed by the bound they belong to.
        //
        // A `Measurement` serializes an absence as a bare `null` and its REASON lives here, in the
        // cell's sibling map - that is this artifact's central convention. So a frontier reading that
        // could not be taken published `null` with its reason nowhere at all until these keys existed:
        // the one state the whole `Measurement` design exists to prevent, reintroduced by a field that
        // is a Vec rather than a scalar and so was not reachable by `absences_of!`.
        //
        // Keyed by the BOUND (`frontier.10ms.rps`) rather than by array index, because the index is an
        // artifact of ordering and the bound is the identity: a reader looking up why the 10ms column is
        // empty should not have to count columns, and inserting a bound later must not renumber the
        // keys of every reading after it.
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
            // NOT recorded: `first_disqualified_conc`. Its absence is not a hole - it is the positive
            // finding that the sweep ran out of range while this bound still held, which the reading's
            // own `lower_bound: true` states directly. Publishing it as an absence too would put one
            // fact in the artifact twice, in two vocabularies, which is what this map exists to avoid.
        }
        out
    }
}

/// Whether/how a cell's streaming path was exercised. Like `Served`, this is a verdict label rather
/// than a number.
///
/// THE VOCABULARY, in full, because it used to be documented as `true` / `false` / `"untestable"`
/// while the producer (`suite::cell_stream`) emitted the token of WHICHEVER `Absent` reason the
/// comparison carried - so `"rig_limited"`, `"below_resolution"`, `"search_exhausted"`,
/// `"harness_error"` and the rest could all appear at a field whose doc named three values. A
/// consumer written against the old doc would treat every one of them as an unknown label:
///
/// - `true`: at least one gateway-vs-direct streaming comparison on this cell produced a number, so
///   frames flowed through the gateway and were timed. `Cell::stream.reason` then names the token of
///   any comparison that did NOT produce one (a cell whose gap figures measured cleanly while the
///   TTFT legs failed is still a served stream, and used to be published as if it were not).
/// - any `Absent` token (`"not_measured"`, `"rig_limited"`, `"untestable"`, ...): the stream was
///   probed and NO comparison produced a number; the token is that absence's own reason, so a rig
///   limit is never published as a claim about the gateway.
/// - `"not_probed"`: the streaming group did not run on this cell at all.
/// - `false`: never written by this engine (a bare `false` would assert the gateway does not stream,
///   which no observation here establishes). Kept representable so older artifacts still parse.
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
    /// The machine-readable REASON TOKEN behind a non-`true` `stream_served`, or behind a `true` one
    /// whose TTFT half is absent - the same vocabulary `Cell::reason` uses, and the same meaning.
    ///
    /// It carried the absence's free-text DETAIL instead, so two fields with one name meant two
    /// different things across the artifact, and a cell whose absence had no detail published a
    /// `null` reason beside a status that plainly had one. The prose has its own field below.
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
    /// The most frames/sec the MOCK can emit at `streams_sustained` concurrency, and the fraction of
    /// it carried. The ceiling here is DERIVED from the mock's declared pacing rather than measured -
    /// see `run::mock_frame_ceiling_fps` - so it is exact arithmetic, and a gateway reaching ~1.0 of it
    /// is forwarding every frame as it arrives, which is the best available outcome rather than a
    /// limit.
    #[serde(default)]
    pub streams_sustained_mock_ceiling: Option<f64>,
    #[serde(default)]
    pub streams_sustained_headroom: Option<f64>,
    /// The sweep points behind `streams_sustained`. Shaped like `SweepPoint` in run.sh's
    /// own accumulator, but no committed snapshot has ever populated it, so each point is passed
    /// through as opaque JSON rather than typed against an unconfirmed shape.
    #[serde(default)]
    pub sweep_streams: Vec<serde_json::Value>,
    #[serde(default)]
    pub stream_c1_note: Option<String>,
    /// HOW MANY TTFT PROBES SURVIVED PER LEG, so a reader can weigh `added_ttft_p50_us` and
    /// `added_ttft_p99_us`. A failed probe was dropped inside a `filter_map`, so a percentile over
    /// three lucky samples published identically to one over a hundred - and with a single survivor the
    /// p50 and p99 ranks collapse to the same index, which reads as a perfectly coherent pair. Mirrors
    /// `CellPerf`'s `gateway_c1_samples`/`direct_c1_samples`, which exist for exactly this.
    #[serde(default = "measurement_default")]
    pub ttft_gw_samples: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub ttft_direct_samples: Measurement<i64>,
}

impl CellStream {
    pub fn absences(&self) -> BTreeMap<String, AbsentEntry> {
        absences_of!(
            self,
            added_ttft_p50_us,
            added_ttft_p99_us,
            added_gap_p50_us,
            added_gap_p99_us,
            ttft_gw_samples,
            ttft_direct_samples,
            streams_sustained,
            streams_sustained_fps,
        )
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

/// The PER-CELL memory window (`CELL_MEM_JSON` in run.sh): its own cold-started, plateau-terminated
/// window for one served cell. Distinct field set from `MatrixMemory` (plateau/growth-rate instead of
/// a single post-load recipe) because it answers a different question: not "what did the peak cell do
/// after everything else ran" but "what does THIS cell do on its own, run cold". No committed snapshot
/// has populated this yet: typed from the shell source, kept permissive by being `Option` throughout.
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
    /// Whether the window settled. A `Measurement`, not an `Option<bool>`, because the three states
    /// are "it settled", "it did not settle" and "the window could not tell" - and the last of those
    /// is an absence with a reason like every other. As an `Option` it published a bare `null` that
    /// `absences()` below could not see (the macro walks `Measurement` fields only), so a served cell
    /// could carry a hole with nothing anywhere saying why: the exact bare null the artifact contract
    /// forbids. The wire form is unchanged - `Measurement` serialises to the bare value or `null`.
    #[serde(default = "measurement_default")]
    pub plateaued: Measurement<bool>,
    #[serde(default = "measurement_default")]
    pub time_to_plateau_s: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub growth_rate_mib_per_min: Measurement<f64>,
    /// How long load was applied. `Measurement` for the same reason `plateaued` is: a window that
    /// never ran has to say so with a reason rather than with a null nothing explains.
    #[serde(default = "measurement_default")]
    pub load_s: Measurement<i64>,
    /// HOW each window failed to settle, when it did: 1 climbing, 0 oscillating, -1 falling, absent
    /// when the window settled and there is no unsettled shape to describe.
    ///
    /// "Did not settle" describes two very different gateways. One climbs without bound; the other
    /// swings around a level it keeps returning to, which is a garbage collector working, not a leak.
    /// Published under one word they are indistinguishable, and the board renders that word as
    /// NEVER SETTLES in red beside a leak rate - an accusation the oscillating gateway did not earn.
    #[serde(default = "measurement_default")]
    pub shape: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub idle_shape: Measurement<f64>,
    #[serde(default, deserialize_with = "null_as_empty_vec")]
    pub rss_series: Vec<RssSample>,
    /// The IDLE window's own readings, so the board can draw what the process did while nothing was
    /// asked of it rather than collapsing a whole minute to one number. Kept apart from
    /// `rss_series` because the two answer different questions: what it costs doing nothing, versus
    /// what work costs it. Empty on a bundle measured before this window existed, which reads as
    /// "not recorded" and draws nothing - never as a flat line.
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
            // Covered here, not merely serialised: these two are the fields whose absence used to
            // publish as a bare null on a served cell because they were plain `Option`s.
            plateaued,
            load_s,
            shape,
            idle_shape,
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
    fn sample_perf() -> CellPerf {
        CellPerf {
            added_latency_p50_us: Measurement::Measured(40_939),
            added_latency_p99_us: Measurement::Measured(40_945),
            gateway_c1_p99_us: Measurement::Measured(41_026),
            direct_c1_p99_us: Measurement::Measured(81),
            // The throughput answer is the FRONTIER now. The six scalars this fixture used to carry -
            // `rps_max_proxy` / `rps_sustained_20ms` and their concurrency twins - were one sweep
            // summarised twice by a chosen ceiling, and they are gone from the artifact.
            frontier: Vec::new(),
            sweep_max_proxy: vec![SweepPoint {
                conc: 256,
                rps: Measurement::Measured(6_209),
                p99_us: Measurement::Measured(43_969),
                fail: Measurement::Measured(0),
            }],
            egress_reverified: Some(true),
            reverify_note: None,
            c1_note: None,
        }
    }

    // THE FRONTIER SURVIVES THE ARTIFACT, MONOTONICITY AND ALL.
    //
    // `frontier.rs` proves the ordering holds over rungs; this proves the published SHAPE keeps it.
    // A serialization that reordered the readings, or dropped the unbounded one, would leave a board
    // whose columns no longer read as a curve while every individual number stayed correct - and the
    // ordering is the whole reason a reader can check us by eye.
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
                rps: Measurement::Measured(*rps),
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
        let rates: Vec<i64> = back.iter().map(|r| r.rps.copied().unwrap()).collect();
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
            rps: Measurement::Measured(19_000),
            concurrency: Measurement::Measured(16_384),
            p99_us: Measurement::Measured(3_000),
            first_disqualified_conc: Measurement::absent(Absent::SearchExhausted),
            lower_bound: true,
        };
        let back: FrontierReading =
            serde_json::from_str(&serde_json::to_string(&r).expect("ser")).expect("de");
        assert_eq!(
            back.rps.copied(),
            Some(19_000),
            "the rate is real and is published"
        );
        assert!(
            back.lower_bound,
            "and it is labelled a floor rather than a ceiling"
        );
        // The REASON does not survive the envelope, and that is this artifact's convention rather than
        // a defect: an absent `Measurement` serializes as a bare null and its reason lives in the
        // CELL's sibling `absences` map. The test that matters is that the map carries it - the next
        // one - and that was a real hole until the frontier's keys were added to `CellPerf::absences`.
        // A Vec field is unreachable by `absences_of!`, so before that every absent reading published
        // a null with its reason nowhere at all: the one state `Measurement` exists to prevent.
        assert_eq!(back.first_disqualified_conc.copied(), None);
    }

    // THE ABSENCE MAP CARRIES EVERY READING'S REASON, KEYED BY ITS BOUND.
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
                rps: Measurement::Measured(19_284),
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

    // ── the published names of the memory fields are a CONTRACT, not an implementation detail ─────
    //
    // These structs are the producer; `site/gen-data.mjs`, `site/seal.mjs` and
    // `site/check-consistency.mjs` are the consumer, and they read these keys by literal name out of
    // the JSON. Nothing in either test suite crossed that boundary: renaming `peak_rss_hwm_mib` here
    // left all 257 Rust tests green, and on the site side the rename does not raise - it reads
    // `undefined`, so C7's `peak_rss_mib <= peak_rss_hwm_mib` invariant silently stops checking and
    // every real peak-RSS number publishes as a null. An absent measurement is supposed to mean the
    // rig could not measure it; here it would mean a field got renamed.
    //
    // Listed literally rather than derived from the struct, on purpose: a test that asked the struct
    // for its own field names would agree with any rename and hold nothing.
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

    // EVERY NULL ON A SERVED CELL CARRIES A REASON. `plateaued` and `load_s` were plain `Option`s, so
    // `absences_of!` - which walks `Measurement` fields - could not see them: a served cell whose
    // memory window could not judge the plateau published a bare null with nothing, anywhere in the
    // artifact, saying why. That is the exact hole the absences map exists to close, and it was open
    // on the two memory fields that are hardest to reason about from the numbers alone.
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
                // The TOKEN in `reason` and the prose in `stream_error`: `reason` used to carry the
                // detail string, which made one field name mean two different things depending on
                // which block a reader was in.
                reason: Some("untestable".to_string()),
                stream_error: Some("did not rebind the mock port".to_string()),
                // NotMeasured, not Untestable: a `null` on the wire always deserialises back to
                // NotMeasured (measurement.rs's Deserialize cannot recover a more specific reason
                // from a bare null), so a round-trippable fixture must start from the reason that
                // survives the trip. The reason-preservation behaviour itself is measurement.rs's
                // own concern and is exercised there, not duplicated here.
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

    // THE REGRESSION THIS PINS: `Cell`'s hand-written `Serialize` lists its fields one by one, so
    // adding a field to the struct does not add it to the wire - the compiler cannot catch a
    // forgotten `st.serialize_field` call the way it would catch a forgotten struct literal field.
    // `timings_s` was added to the struct, filled by the suite ("Cost belongs in the artifact"),
    // and dropped by this exact omission earlier today; every other test here builds a `Cell` and
    // only checks the fields it already knows to look for, so all 15 of this module's tests and all
    // 11 of `artifact_contract.rs` stayed green with the line missing. This test looks at the one
    // field none of them asserted on.
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

    // THE TOKEN AND THE PROSE ARE TWO FIELDS. `CellStream.reason` shares its name with `Cell.reason`,
    // which every consumer reads as a stable token, while the stream block put the absence's free
    // text there instead - so branching on `reason` gave a token in one block and a sentence in the
    // other, and a reasonless absence published a null beside a status that plainly had a reason.
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

    // The invariants below are properties of the TYPES, so they are asserted against a snapshot
    // built right here, not by scanning results/snapshots/ and asserting on whatever is committed
    // there: a test that a `git rm` can fail is measuring the wrong thing. Whether real artifacts
    // parse is a stronger claim and is covered where it belongs: engine/tests/end_to_end.rs drives
    // the real binary and reads back the snapshot it actually wrote.

    // The other half: a cell that was not served must not arrive carrying numbers. Fabricating a
    // perf block for an unserved cell would publish a capability the gateway never demonstrated.
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
