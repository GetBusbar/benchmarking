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
fn is_false(value: &bool) -> bool {
    !*value
}

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
    /// Identity of the source-built gateway binary, recorded by its successful build.
    /// Older snapshots and image gateways have no inferred or fabricated build receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_provenance: Option<serde_json::Value>,
    /// WHAT EACH PUBLISHED METRIC MEANS, keyed by metric, built from the engine's own constants.
    ///
    /// A number a reader cannot check is a number they have to trust, and this project's whole claim is
    /// that they should not have to. Each entry states the quantity, the predicate that decides which
    /// observations count, and how the measurement terminates - so "is this the peak?" is answerable from
    /// the artifact instead of from us.
    ///
    /// TOP-LEVEL, NOT PER CELL, because a definition does not vary by cell: repeating it across 36 cells
    /// would be bloat, and worse, would create 36 places for one statement to drift.
    ///
    /// Formatted from the constants themselves rather than hand-written, for the reason the memory
    /// `protocol` string learned the hard way: every published surface described the retired throughput
    /// gate as "p99 < 1 s" while the engine enforced 20 ms, a bar 96% of recorded rungs pass versus 57%.
    /// A definition maintained by hand is a definition that will disagree with the code.
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
    /// `served` is OBSERVED, not asserted: true only if at least one cell probed through this egress
    /// was served (`suite.rs` ORs it over the column's cells). A column whose every cell was
    /// untestable, refused or not configured reads `false`.
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
    /// FRACTIONAL BELOW 1/s, whole at or above it - see `GenStats::rps`. Published as a float so a
    /// cell serving 0.25 req/s cannot report `0`, which says it carried nothing.
    #[serde(default = "measurement_default")]
    pub rps: Measurement<f64>,
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
    /// SUCCESSFUL COMPLETIONS IN THIS WINDOW. Published so a reader - or an independent checker - can
    /// apply the engine's own "served cleanly" rule (`frontier::Rung::served_cleanly`: `ok > 0 &&
    /// fail == 0`) EXACTLY, rather than approximating it.
    ///
    /// It was not published, and that cost a real false alarm. `bench-audit.py` had to stand in
    /// `rps > 0` for `ok > 0`, and plano at c=256 published `rps: 0, p99_us: 3398432, fail: 0` - which
    /// that rule calls dirty. But a percentile cannot exist without a completed, timed request, so
    /// `ok >= 1` and the window was clean; its RATE merely rounded down through the `as i64` below (one
    /// request over a four-second window is 0.25 rps). The audit then re-derived a
    /// `first_disqualified_conc` the engine had correctly left absent, and the disagreement had to be
    /// settled BY HAND against the raw numbers.
    ///
    /// Hand-settling a disagreement between two checkers is not a solution: it does not scale, and the
    /// next occurrence may have nobody watching. The input the rule needs is now in the artifact.
    #[serde(default = "measurement_default")]
    pub ok: Measurement<i64>,
    /// See the frontier reading's `rps`: fractional below 1/s so a rung that completed one request in
    /// four seconds is not published as having carried nothing.
    #[serde(default = "measurement_default")]
    pub rps: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub fail: Measurement<i64>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellPerf {
    /// Partial windows are retained as evidence; their sweep did not complete.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sweep_max_proxy_interrupted: bool,
    #[serde(default = "measurement_default")]
    pub added_latency_p50_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub added_latency_p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub gateway_c1_p99_us: Measurement<i64>,
    #[serde(default = "measurement_default")]
    pub direct_c1_p99_us: Measurement<i64>,
    /// WHAT A REQUEST COST, in microseconds of gateway CPU.
    ///
    /// The throughput answer stops describing the gateway the moment it saturates its pinned cores;
    /// this does not. At saturation two gateways deliver the same rps by definition, and the one
    /// doing less work per request still reads lower here - which is also the figure that maps to
    /// money, since half the CPU serves the same traffic on half the instance.
    ///
    /// ABSENT ON EVERY SNAPSHOT TAKEN BEFORE THIS EXISTED, and absent is the only honest rendering:
    /// a 0 would make a gateway that was never measured look infinitely efficient.
    #[serde(default = "measurement_default")]
    pub cpu_us_per_request: Measurement<f64>,
    /// The same fact the way an operator sizes a box: requests served per second of CPU burned.
    #[serde(default = "measurement_default")]
    pub rps_per_cpu_second: Measurement<f64>,
    /// The concurrency the two above were taken at, published because it is what was HELD CONSTANT.
    /// No single concurrency is sub-saturation across a field spanning 19 to 49,000 rps, so matched
    /// concurrency is the honest substitute for matched load - and a reader has to be able to see
    /// which it was rather than trust that something was.
    #[serde(default = "measurement_default")]
    pub cost_window_conc: Measurement<i64>,

    /// Requests the cost window actually completed, and the rate it carried.
    ///
    /// PUBLISHED SO THE COST IS CHECKABLE. `cpu_us_per_request` is CPU divided by this count, and
    /// without it the figure re-derives from nothing. It also separates two opposite readings of a
    /// low utilisation - a genuinely cheap gateway, or a window that simply carried less load than
    /// the sweep did at the same concurrency - which are indistinguishable without knowing the load.
    #[serde(default = "measurement_default")]
    pub cost_window_ok: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub cost_window_rps: Measurement<f64>,
    /// Utilisation of the cores the gateway was PINNED to, across the cost window. 1.0 = every
    /// pinned core fully busy.
    ///
    /// THIS IS WHAT MAKES A PEAK INTERPRETABLE. A throughput number alone cannot say whether a
    /// gateway stopped because it ran out of CPU or because something else bound it - and reading
    /// that off the shape of a latency curve is how the ceiling was called wrong twice on
    /// 2026-07-30. Near 1.0 the peak is a real wall; well below it, the wall is elsewhere.
    #[serde(default = "measurement_default")]
    pub cost_core_utilisation: Measurement<f64>,
    #[serde(default = "measurement_default")]
    /// Threads in the gateway's tree during the cost window. Not a rate: it is the shape of the
    /// concurrency model, thread-per-connection against async, on evidence rather than on a claim.
    /// (This paragraph sat on `cost_window_ok` - a completed-request COUNT - so one field's doc
    /// stated two mutually exclusive definitions while this one had none.)
    pub cost_threads: Measurement<f64>,
    /// Involuntary context switches per request - the scheduler taking the CPU away, which is what a
    /// saturated core looks like from inside the process. The best single explainer of a tail.
    #[serde(default = "measurement_default")]
    pub cost_nonvol_ctxt_per_request: Measurement<f64>,
    /// Major faults during the cost window. NON-ZERO MEANS THE BOX WAS SWAPPING, so the window timed
    /// the disk; the cost figures above are re-flagged as a harness fault when this is not 0, and
    /// this number publishes so a reader sees WHY rather than finding a hole.
    #[serde(default = "measurement_default")]
    pub cost_majflt: Measurement<f64>,
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
            // THE COST FIELDS, and leaving them out reintroduced the exact bug the comment below
            // describes. They published a bare `null` with the reason nowhere - which for cost is
            // worse than for most fields, because the two most likely reasons are REFUSALS the
            // reader must see: the window had failures, or the box was swapping. An unexplained
            // null reads as "not implemented"; the truth was "measured, and deliberately withheld".
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
        /* AND EVERY RUNG'S OWN ABSENCES, for exactly the reason the frontier's are here.
        `sweep_max_proxy` is a Vec of `SweepPoint`s whose `ok`/`rps`/`p99_us`/`fail` are
        `Measurement`s, so `absences_of!` cannot reach them and nothing else walked the Vec: a rung
        the generator returned no window for (`search`'s default `reading: None`) published
        `{"conc":1024,"ok":null,"rps":6209.0,"p99_us":null,"fail":null}` with its reasons nowhere.
        Three bare nulls - and these are the very inputs `frontier::Rung::served_cleanly` is
        published for, so an external checker reading `ok: null` could not tell "no requests
        completed" from "no window was recorded", which is the distinction that cost a real false
        alarm on plano and had to be settled by hand.

        Keyed by CONCURRENCY and its window ordinal rather than by array index, on the frontier's
        own reasoning: the index is an artifact of ordering, while c=1024's second window is an
        identity that survives a rung being inserted before it. The `absences_of!` guard cannot
        police a Vec, so this is the only place these can be stated. */
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
    /// the `gateway_c1_samples`/`direct_c1_samples` METRIC-SURFACE keys, which exist for exactly
    /// this. They are not `CellPerf` fields and never reach the wire - `suite` consumes them only to
    /// format `c1_note` - so the sample weight behind the added-latency percentiles is recoverable
    /// from the prose and not from a field.
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
        // THE TWO DERIVED FIELDS INHERIT THEIR PARENT'S REASON, because they cannot state one.
        //
        // `streams_sustained_mock_ceiling` and `_headroom` are `Option<f64>`, not `Measurement`, so
        // they have no slot for a reason and `absences_of!` cannot cover them. On the 2026-07-30 board
        // that left them null on 64 cells with NO entry in the cell's absences map - nulls nobody had
        // to explain, which is the one thing this artifact is not supposed to contain.
        //
        // They are DERIVED from `streams_sustained` (headroom is the carried fraction of the ceiling),
        // so when the parent is absent they are absent for precisely the parent's reason and copying it
        // is a statement of fact rather than an invented one. Converting them to `Measurement` would be
        // the tidier fix and would also let them carry a reason of their OWN - worth doing if a case
        // ever appears where they are absent while the parent is present.
        /* THE CASE THE PARAGRAPH ABOVE SAID TO WATCH FOR HAS APPEARED, so the guard no longer hangs
        off the parent being absent. `suite` produces both of these as `None` while
        `streams_sustained` is MEASURED, twice: a bisect that lands on conc == 0 publishes
        sustained 0 with no ceiling and no headroom, and `stream_rig_ceiling` returns an absence
        whenever the mock's derived frame ceiling is not positive, which leaves `reference.copied()`
        None and `rigbound::headroom` None beside a measured concurrency.

        Both were bare nulls on a served cell - the one thing this artifact is not supposed to
        contain - and they slipped through precisely because the old condition asked about the
        parent rather than about the fields themselves. */
        // Each key paired with its own field's state, in one place: a key list and a separate match
        // back onto the fields were two spellings of one fact, and a third key added to the list
        // alone would have been judged by the wildcard arm's field.
        for (key, missing) in [
            (
                "streams_sustained_mock_ceiling",
                self.streams_sustained_mock_ceiling.is_none(),
            ),
            (
                "streams_sustained_headroom",
                self.streams_sustained_headroom.is_none(),
            ),
        ] {
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
    /* THE IDLE-LEAK VERDICT AND ITS RATE. `Memory` declares twelve fields and measures all twelve;
    `CellMemory` carried ten, and these were the two with nowhere to land - so they were computed
    on every served cell and then dropped, with no field, no null and no absence entry. The board
    renders "at rest, before any load" with no verdict because `site/app.js` reads exactly these
    two names and gets `undefined`.

    A gateway whose RSS climbs at idle - which `metric.rs` calls the most damning memory result
    there is - published identically to one holding perfectly flat. A missing key and a null mean
    different things, and only one of them is honest. */
    #[serde(default = "measurement_default")]
    pub idle_static: Measurement<f64>,
    #[serde(default = "measurement_default")]
    pub idle_growth_rate_mib_per_min: Measurement<f64>,
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
#[path = "tests/record_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/record_derived_stream_absence_tests.rs"]
mod derived_stream_absence_tests;

#[cfg(test)]
#[path = "tests/record_sweep_rung_absence_tests.rs"]
mod sweep_rung_absence_tests;
