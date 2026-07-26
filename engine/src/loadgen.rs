// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE UGEN DRIVER, ported from lib/sweep.sh's sweep_probe/sweep_probe_raw and
// lib/stream_measure.sh's stream_probe.
//
// Two rules carry the whole module. First, a hard timeout is mandatory: the shell wraps every
// invocation in `tmo` at dur+grace because an unresponsive gateway otherwise blocks the whole
// suite on ugen's own tail timeouts, and a timeout kill means "we did not get a reading", not "the
// gateway measured zero". Second, empty or malformed output is unmeasured, never a zero and never a
// defaulted field: the shell equivalent of getting this wrong (`rps="${rps:-0}"`) once published an
// unprobed rung as a measured zero. Argument construction and stats-line parsing are pure functions
// so both can be driven by tests without ugen existing on the machine; the process spawn is a thin
// wrapper around them.

use crate::measurement::{Absent, Measurement};
use crate::search::{Probe, Sample};
use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Seconds added to a probe's own duration for its hard timeout. Matches the shell's
/// `HARNESS_PROBE_GRACE` default: slack for warm-up, connection teardown, and ugen's own tail.
pub const PROBE_GRACE_S: u32 = 45;

/// The budget (in seconds) a single probe of length `duration_s` is allowed before it is killed as
/// hung. Pure so the timeout math is testable without touching a clock or a process.
pub fn probe_budget(duration_s: u32) -> Duration {
    Duration::from_secs((duration_s + PROBE_GRACE_S) as u64)
}

/// The invariant parameters of every ugen invocation for one gateway under test: everything sweep.sh
/// and stream_measure.sh thread through unchanged across a whole sweep or search.
#[derive(Debug, Clone)]
pub struct LoadgenConfig {
    /// Path to the prebuilt ugen binary.
    pub ugen_path: String,
    /// CPU list passed to `taskset -c`, pinning the load generator to its own cores so it never
    /// competes with the gateway under test for CPU time.
    pub load_cores: String,
    pub model: String,
    pub auth: String,
    pub psize: u32,
    /// Extra `-H "Key: Value"` headers, repeated in order.
    pub extra_headers: Vec<String>,
    /// A verbatim request body for every request (the matrix suite's exact ingress-dialect probe
    /// body). `None`, or an empty string, omits `-body` entirely so ugen builds its own `-shape`
    /// body, exactly as the shell does: `[ -n "$SWEEP_BODY" ] && _sb=(-body "$SWEEP_BODY")`.
    pub body: Option<String>,
}

/// Extra flags the streaming lane adds over the throughput lane.
#[derive(Debug, Clone, Copy)]
pub struct StreamParams {
    /// Expected content frames per stream (the `delivered` denominator; 0 = ugen skips it).
    pub expframes: u32,
    /// Per-stream stall threshold in microseconds (0 = off).
    pub stall_us: u64,
}

/// Append `-body <body>` iff a non-empty body was supplied, and then the extra headers in order.
/// Shared tail for both lanes so the "omit rather than pass empty" rule lives in one place.
fn push_body_and_headers(args: &mut Vec<String>, cfg: &LoadgenConfig) {
    if let Some(body) = cfg.body.as_deref().filter(|b| !b.is_empty()) {
        args.push("-body".to_string());
        args.push(body.to_string());
    }
    for h in &cfg.extra_headers {
        args.push("-H".to_string());
        args.push(h.clone());
    }
}

/// The ugen argument list for the throughput lane, matching `sweep_probe_raw`'s invocation:
/// `-url ... -model ... -auth ... -c ... -d ... -psize ... [-body ...] [-H ...]*`.
pub fn throughput_args(cfg: &LoadgenConfig, url: &str, concurrency: u32, duration_s: u32) -> Vec<String> {
    let mut args = vec![
        "-url".to_string(),
        url.to_string(),
        "-model".to_string(),
        cfg.model.clone(),
        "-auth".to_string(),
        cfg.auth.clone(),
        "-c".to_string(),
        concurrency.to_string(),
        "-d".to_string(),
        duration_s.to_string(),
        "-psize".to_string(),
        cfg.psize.to_string(),
    ];
    push_body_and_headers(&mut args, cfg);
    args
}

/// The ugen argument list for the streaming lane, matching `stream_probe`'s invocation: the
/// throughput flags plus `-stream -expframes ... -stallus ...`, then the same body/header tail.
pub fn stream_args(
    cfg: &LoadgenConfig,
    url: &str,
    concurrency: u32,
    duration_s: u32,
    stream: StreamParams,
) -> Vec<String> {
    let mut args = vec![
        "-url".to_string(),
        url.to_string(),
        "-model".to_string(),
        cfg.model.clone(),
        "-auth".to_string(),
        cfg.auth.clone(),
        "-c".to_string(),
        concurrency.to_string(),
        "-d".to_string(),
        duration_s.to_string(),
        "-psize".to_string(),
        cfg.psize.to_string(),
        "-stream".to_string(),
        "-expframes".to_string(),
        stream.expframes.to_string(),
        "-stallus".to_string(),
        stream.stall_us.to_string(),
    ];
    push_body_and_headers(&mut args, cfg);
    args
}

/// Build the full command: `taskset -c <load_cores> <ugen_path> <args...>`. CPU pinning is applied
/// here, unconditionally, exactly as the shell's `taskset -c "$LOADCORES" "$UGEN" ...`.
fn pinned_command(cfg: &LoadgenConfig, args: &[String]) -> Command {
    let mut cmd = Command::new("taskset");
    cmd.arg("-c").arg(&cfg.load_cores).arg(&cfg.ugen_path).args(args);
    cmd
}

/// Split a ugen stats line into its `k=v` pairs. Unrecognised keys are kept (and simply never
/// looked up), which is how an unfamiliar future field is ignored rather than rejected.
fn parse_kv(line: &str) -> BTreeMap<&str, &str> {
    line.split_whitespace().filter_map(|tok| tok.split_once('=')).collect()
}

fn require_i64(fields: &BTreeMap<&str, &str>, key: &str, line: &str) -> Result<i64, String> {
    match fields.get(key) {
        None => Err(format!("missing field '{key}' in stats line: '{line}'")),
        Some(v) => v.parse::<i64>().map_err(|_| format!("non-numeric field '{key}={v}' in stats line: '{line}'")),
    }
}

fn require_f64(fields: &BTreeMap<&str, &str>, key: &str, line: &str) -> Result<f64, String> {
    match fields.get(key) {
        None => Err(format!("missing field '{key}' in stats line: '{line}'")),
        Some(v) => v.parse::<f64>().map_err(|_| format!("non-numeric field '{key}={v}' in stats line: '{line}'")),
    }
}

/// The throughput lane's stats, from ugen's line:
/// `rps=%d fail=%d p50=%.2f p99=%.2f p50us=%d p99us=%d ok=%d`.
/// The `p50`/`p99` millisecond floats are redundant with the microsecond fields the shell actually
/// reads and are dropped here; `ok` is kept because `sweep_probe_raw` exists specifically so a
/// caller can read it.
#[derive(Debug, Clone, PartialEq)]
pub struct UgenStats {
    pub rps: i64,
    pub fail: i64,
    pub p50_us: i64,
    pub p99_us: i64,
    pub ok: i64,
}

fn parse_ugen_fields(line: &str) -> Result<UgenStats, String> {
    let fields = parse_kv(line);
    Ok(UgenStats {
        rps: require_i64(&fields, "rps", line)?,
        fail: require_i64(&fields, "fail", line)?,
        p50_us: require_i64(&fields, "p50us", line)?,
        p99_us: require_i64(&fields, "p99us", line)?,
        ok: require_i64(&fields, "ok", line)?,
    })
}

/// Parse one ugen throughput stats line. Empty input is `Absent(NotMeasured)`: ugen printed
/// nothing, which is what a timeout kill, a crash, or a hard-down gateway all look like, and none
/// of them is a zero. A present line missing a required field, or carrying a non-numeric one, is
/// `Absent(HarnessError)` with the offending text in the detail: a stats line in an unexpected shape
/// is a contract break with ugen, never a value to default.
pub fn parse_ugen_line(raw: &str) -> Measurement<UgenStats> {
    let line = raw.trim();
    if line.is_empty() {
        return Measurement::absent(Absent::NotMeasured);
    }
    match parse_ugen_fields(line) {
        Ok(s) => Measurement::Measured(s),
        Err(detail) => Measurement::absent_because(Absent::HarnessError, detail),
    }
}

/// The streaming lane's stats, from ugen's line:
/// `streams=%d complete=%d fail=%d stalled=%d frames=%d fps=%d delivered=%.4f
///  ttft_p50us=%d ttft_p99us=%d gap_p50us=%d gap_p99us=%d`.
/// `delivered` can legitimately print as `NaN` (ugen sets it to `math.NaN()` when `-expframes` was
/// not given or nothing started): that parses to a real `f64::NAN`, a measured "not applicable", not
/// a parse failure.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamStats {
    pub streams: i64,
    pub complete: i64,
    pub fail: i64,
    pub stalled: i64,
    pub frames: i64,
    pub fps: i64,
    pub delivered: f64,
    pub ttft_p50_us: i64,
    pub ttft_p99_us: i64,
    pub gap_p50_us: i64,
    pub gap_p99_us: i64,
}

fn parse_stream_fields(line: &str) -> Result<StreamStats, String> {
    let fields = parse_kv(line);
    Ok(StreamStats {
        streams: require_i64(&fields, "streams", line)?,
        complete: require_i64(&fields, "complete", line)?,
        fail: require_i64(&fields, "fail", line)?,
        stalled: require_i64(&fields, "stalled", line)?,
        frames: require_i64(&fields, "frames", line)?,
        fps: require_i64(&fields, "fps", line)?,
        delivered: require_f64(&fields, "delivered", line)?,
        ttft_p50_us: require_i64(&fields, "ttft_p50us", line)?,
        ttft_p99_us: require_i64(&fields, "ttft_p99us", line)?,
        gap_p50_us: require_i64(&fields, "gap_p50us", line)?,
        gap_p99_us: require_i64(&fields, "gap_p99us", line)?,
    })
}

/// Parse one ugen streaming stats line. Same absence rules as `parse_ugen_line`: empty is
/// `NotMeasured`, a malformed present line is `HarnessError` with the offending text attached.
pub fn parse_stream_line(raw: &str) -> Measurement<StreamStats> {
    let line = raw.trim();
    if line.is_empty() {
        return Measurement::absent(Absent::NotMeasured);
    }
    match parse_stream_fields(line) {
        Ok(s) => Measurement::Measured(s),
        Err(detail) => Measurement::absent_because(Absent::HarnessError, detail),
    }
}

/// Run `cmd`, capturing stdout, but kill it and return `None` if it has not finished within
/// `budget`. `None` covers both "never finished" and "could not even be spawned or read": in both
/// cases there is no stats line to parse, which is the only fact this function needs to report.
/// Deliberately a direct kill rather than the shell's TERM-then-KILL grace: ugen holds no state a
/// graceful shutdown would protect, so there is nothing for the grace period to buy here.
fn spawn_with_timeout(mut cmd: Command, budget: Duration) -> Option<String> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        use std::io::Read;
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        let _ = tx.send(buf);
    });
    match rx.recv_timeout(budget) {
        Ok(buf) => {
            let _ = child.wait();
            Some(buf)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

/// The ugen driver for one gateway under test: invariant config in, typed stats out.
pub struct Loadgen {
    pub config: LoadgenConfig,
}

impl Loadgen {
    pub fn new(config: LoadgenConfig) -> Self {
        Loadgen { config }
    }

    /// Throughput lane: one hard-timeout-wrapped ugen invocation, parsed into `UgenStats`.
    pub fn run(&self, url: &str, concurrency: u32, duration_s: u32) -> Measurement<UgenStats> {
        let args = throughput_args(&self.config, url, concurrency, duration_s);
        let cmd = pinned_command(&self.config, &args);
        match spawn_with_timeout(cmd, probe_budget(duration_s)) {
            None => Measurement::absent_because(
                Absent::NotMeasured,
                format!("ugen did not return within {}s (c={concurrency}, d={duration_s}s)", probe_budget(duration_s).as_secs()),
            ),
            Some(out) => parse_ugen_line(&out),
        }
    }

    /// Streaming lane: same timeout discipline, parsed into `StreamStats`.
    pub fn run_stream(&self, url: &str, concurrency: u32, duration_s: u32, stream: StreamParams) -> Measurement<StreamStats> {
        let args = stream_args(&self.config, url, concurrency, duration_s, stream);
        let cmd = pinned_command(&self.config, &args);
        match spawn_with_timeout(cmd, probe_budget(duration_s)) {
            None => Measurement::absent_because(
                Absent::NotMeasured,
                format!("ugen did not return within {}s (c={concurrency}, d={duration_s}s, stream)", probe_budget(duration_s).as_secs()),
            ),
            Some(out) => parse_stream_line(&out),
        }
    }
}

/// The sustained-throughput gate: error rate under `max_fail_fraction` (0.1% by default, not
/// literal zero: a single failure in tens of thousands should not zero a gateway's result) AND p99
/// under `p99_ceiling_us`. Mirrors `_sw_pass` / `_sw_pass_c` in lib/sweep.sh.
#[derive(Debug, Clone, Copy)]
pub struct RpsGate {
    pub p99_ceiling_us: i64,
    pub max_fail_fraction: f64,
}

impl RpsGate {
    pub fn passed(&self, stats: &UgenStats, duration_s: u32) -> bool {
        let total = stats.rps * i64::from(duration_s) + stats.fail;
        total > 0 && (stats.fail as f64) <= self.max_fail_fraction * (total as f64) && stats.p99_us < self.p99_ceiling_us
    }
}

/// The streaming sustained/cpu-fps gate: real streams, error rate under `max_fail_fraction`,
/// `delivered` at or above `min_delivered`, zero stalled, and (cpu-fps only) at least
/// `min_complete_fraction` of streams completed. Mirrors `_sm_probe_c`'s gate in
/// lib/stream_measure.sh; `min_complete_fraction = 0.0` reproduces the sustained lane's no-op floor.
#[derive(Debug, Clone, Copy)]
pub struct StreamGate {
    pub min_delivered: f64,
    pub max_fail_fraction: f64,
    pub min_complete_fraction: f64,
}

impl StreamGate {
    pub fn passed(&self, stats: &StreamStats) -> bool {
        stats.streams > 0
            && (stats.fail as f64) <= self.max_fail_fraction * (stats.streams as f64)
            && stats.delivered >= self.min_delivered
            && stats.stalled == 0
            && (stats.complete as f64) >= self.min_complete_fraction * (stats.streams as f64)
    }
}

/// Drives the throughput lane as a `Probe`, so `bisect_ceiling` (sustained rps ceiling) and
/// `peak_max` (peak rps) run it unchanged. `probe` returning `None` covers both a suite-deadline
/// style interruption and ugen's own hard-timeout kill: a search that sees `None` treats it as "the
/// window produced nothing", which is exactly what both of those are. A malformed-but-present stats
/// line is a harness contract break rather than an interruption, so it is logged loudly to stderr
/// (the `Probe` trait has no channel for a detail string) before also reporting `None`, so a search
/// halts on it rather than fabricating a number from a broken line.
pub struct RpsProbe<'a> {
    pub loadgen: &'a Loadgen,
    pub url: String,
    pub duration_s: u32,
    pub gate: RpsGate,
}

impl Probe for RpsProbe<'_> {
    fn probe(&mut self, concurrency: u32) -> Option<Sample> {
        match self.loadgen.run(&self.url, concurrency, self.duration_s) {
            Measurement::Measured(stats) => {
                let passed = self.gate.passed(&stats, self.duration_s);
                Some(Sample { value: stats.rps as f64, passed })
            }
            Measurement::Absent { reason, detail } => {
                if reason != Absent::NotMeasured {
                    eprintln!(
                        "loadgen: rps probe at c={concurrency} produced no usable stats ({reason}): {}",
                        detail.unwrap_or_default()
                    );
                }
                None
            }
        }
    }
}

/// Drives the streaming lane as a `Probe` (fps is the value both `stream_sustained_bisect` and
/// `streamcpu_peak_fps` maximise/gate on in the shell original), with the same `None` semantics as
/// `RpsProbe`.
pub struct StreamProbe<'a> {
    pub loadgen: &'a Loadgen,
    pub url: String,
    pub duration_s: u32,
    pub stream: StreamParams,
    pub gate: StreamGate,
}

impl Probe for StreamProbe<'_> {
    fn probe(&mut self, concurrency: u32) -> Option<Sample> {
        match self.loadgen.run_stream(&self.url, concurrency, self.duration_s, self.stream) {
            Measurement::Measured(stats) => {
                let passed = self.gate.passed(&stats);
                Some(Sample { value: stats.fps as f64, passed })
            }
            Measurement::Absent { reason, detail } => {
                if reason != Absent::NotMeasured {
                    eprintln!(
                        "loadgen: stream probe at c={concurrency} produced no usable stats ({reason}): {}",
                        detail.unwrap_or_default()
                    );
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(body: Option<&str>) -> LoadgenConfig {
        LoadgenConfig {
            ugen_path: "/opt/ugen".to_string(),
            load_cores: "0-1".to_string(),
            model: "gpt-4o-mini".to_string(),
            auth: "sk-dummy".to_string(),
            psize: 256,
            extra_headers: vec!["X-Extra: 1".to_string()],
            body: body.map(str::to_string),
        }
    }

    // ── argument construction ───────────────────────────────────────────────────────────────────

    #[test]
    fn throughput_args_match_sweep_probe_raw_without_a_body() {
        let c = cfg(None);
        let args = throughput_args(&c, "http://gw/v1/chat", 64, 12);
        assert_eq!(
            args,
            vec![
                "-url", "http://gw/v1/chat", "-model", "gpt-4o-mini", "-auth", "sk-dummy", "-c", "64", "-d", "12",
                "-psize", "256", "-H", "X-Extra: 1",
            ]
        );
        assert!(!args.contains(&"-body".to_string()), "no body was supplied, -body must be entirely absent");
    }

    #[test]
    fn throughput_args_thread_a_supplied_body() {
        let c = cfg(Some(r#"{"model":"m","stream":true}"#));
        let args = throughput_args(&c, "http://gw/v1/chat", 64, 12);
        let bi = args.iter().position(|a| a == "-body").expect("-body must be present when a body is configured");
        assert_eq!(args[bi + 1], r#"{"model":"m","stream":true}"#);
    }

    #[test]
    fn an_empty_body_is_omitted_like_no_body_at_all() {
        let c = cfg(Some(""));
        let args = throughput_args(&c, "http://gw/v1/chat", 64, 12);
        assert!(!args.contains(&"-body".to_string()), "an empty body string must omit -body, matching the shell's -n test");
    }

    #[test]
    fn stream_args_add_the_streaming_flags_in_shell_order() {
        let c = cfg(None);
        let args = stream_args(&c, "http://gw/v1/stream", 32, 20, StreamParams { expframes: 40, stall_us: 500_000 });
        assert_eq!(
            args,
            vec![
                "-url", "http://gw/v1/stream", "-model", "gpt-4o-mini", "-auth", "sk-dummy", "-c", "32", "-d", "20",
                "-psize", "256", "-stream", "-expframes", "40", "-stallus", "500000", "-H", "X-Extra: 1",
            ]
        );
    }

    #[test]
    fn stream_args_thread_the_ingress_dialect_body_before_headers() {
        let c = cfg(Some(r#"{"stream":true}"#));
        let args = stream_args(&c, "http://gw/v1/stream", 32, 20, StreamParams { expframes: 40, stall_us: 0 });
        let bi = args.iter().position(|a| a == "-body").expect("-body must be present");
        assert_eq!(args[bi + 1], r#"{"stream":true}"#);
        let hi = args.iter().position(|a| a == "-H").expect("-H must still be present");
        assert!(bi < hi, "body precedes headers, matching stream_probe's argument order");
    }

    #[test]
    fn pinned_command_applies_the_configured_cpu_list() {
        let c = cfg(None);
        let args = throughput_args(&c, "http://gw", 1, 1);
        let cmd = pinned_command(&c, &args);
        // Command exposes no arg accessor, so assert through its Debug rendering: taskset -c
        // <load_cores> <ugen_path> must all be present, and in that order.
        let rendered = format!("{cmd:?}");
        let cores_at = rendered.find("0-1").expect("load_cores must appear");
        let ugen_at = rendered.find("/opt/ugen").expect("ugen_path must appear");
        assert!(rendered.starts_with("\"taskset\""), "must invoke taskset: {rendered}");
        assert!(cores_at < ugen_at, "cpu pin must precede the ugen binary: {rendered}");
    }

    // ── throughput stats parsing ────────────────────────────────────────────────────────────────

    // Format taken verbatim from ugen.go's non-stream Printf, not from the shell's awk parser.
    const UGEN_LINE: &str = "rps=1234 fail=3 p50=12.50 p99=45.00 p50us=12500 p99us=45000 ok=14808";

    #[test]
    fn a_real_ugen_line_parses_every_field() {
        let m = parse_ugen_line(UGEN_LINE);
        assert_eq!(
            m,
            Measurement::Measured(UgenStats { rps: 1234, fail: 3, p50_us: 12_500, p99_us: 45_000, ok: 14_808 })
        );
    }

    #[test]
    fn empty_ugen_output_is_not_measured_never_a_zero() {
        for empty in ["", "   ", "\n"] {
            let m = parse_ugen_line(empty);
            assert!(!m.is_measured());
            assert_eq!(m.reason(), Some(&Absent::NotMeasured));
        }
    }

    #[test]
    fn a_missing_field_is_absent_with_the_text_in_the_detail_never_a_default() {
        let line = "rps=1234 fail=3 p99us=45000 ok=14808"; // p50us dropped
        let m = parse_ugen_line(line);
        assert!(!m.is_measured());
        assert_eq!(m.reason(), Some(&Absent::HarnessError));
        assert!(m.detail().unwrap_or_default().contains(line), "detail must carry the offending line");
        assert!(m.detail().unwrap_or_default().contains("p50us"));
    }

    #[test]
    fn a_non_numeric_field_is_absent_never_silently_parsed_as_zero() {
        let line = "rps=1234 fail=abc p50us=12500 p99us=45000 ok=14808";
        let m = parse_ugen_line(line);
        assert!(!m.is_measured());
        assert_eq!(m.reason(), Some(&Absent::HarnessError));
        assert!(m.detail().unwrap_or_default().contains("fail=abc"));
    }

    #[test]
    fn a_genuine_rps_zero_line_is_a_measured_zero_distinct_from_absence() {
        let line = "rps=0 fail=0 p50=0.00 p99=0.00 p50us=0 p99us=0 ok=0";
        let m = parse_ugen_line(line);
        assert!(m.is_measured());
        assert_eq!(m.value().map(|s| s.rps), Some(0));
    }

    #[test]
    fn unknown_extra_fields_are_ignored_not_a_parse_failure() {
        let line = format!("{UGEN_LINE} pad=64 futurefield=x");
        let m = parse_ugen_line(&line);
        assert!(m.is_measured());
    }

    // ── streaming stats parsing ─────────────────────────────────────────────────────────────────

    // Format taken verbatim from ugen.go's stream-mode Printf.
    const STREAM_LINE: &str =
        "streams=64 complete=63 fail=1 stalled=0 frames=2048 fps=170 delivered=0.9995 ttft_p50us=8000 ttft_p99us=21000 gap_p50us=500 gap_p99us=1200";

    #[test]
    fn a_real_stream_line_parses_every_field() {
        let m = parse_stream_line(STREAM_LINE);
        assert_eq!(
            m,
            Measurement::Measured(StreamStats {
                streams: 64,
                complete: 63,
                fail: 1,
                stalled: 0,
                frames: 2048,
                fps: 170,
                delivered: 0.9995,
                ttft_p50_us: 8_000,
                ttft_p99_us: 21_000,
                gap_p50_us: 500,
                gap_p99_us: 1_200,
            })
        );
    }

    #[test]
    fn empty_stream_output_is_not_measured() {
        let m = parse_stream_line("");
        assert!(!m.is_measured());
        assert_eq!(m.reason(), Some(&Absent::NotMeasured));
    }

    #[test]
    fn a_missing_stream_field_is_absent_with_detail() {
        let line = "streams=64 complete=63 fail=1 stalled=0 frames=2048 fps=170 delivered=0.9995 ttft_p99us=21000 gap_p50us=500 gap_p99us=1200"; // ttft_p50us dropped
        let m = parse_stream_line(line);
        assert!(!m.is_measured());
        assert_eq!(m.reason(), Some(&Absent::HarnessError));
        assert!(m.detail().unwrap_or_default().contains("ttft_p50us"));
    }

    #[test]
    fn a_non_numeric_stream_field_is_absent() {
        let line = STREAM_LINE.replace("stalled=0", "stalled=nope");
        let m = parse_stream_line(&line);
        assert!(!m.is_measured());
        assert_eq!(m.reason(), Some(&Absent::HarnessError));
    }

    #[test]
    fn a_genuine_zero_fps_stream_line_is_measured_not_absent() {
        let line = STREAM_LINE.replace("fps=170", "fps=0");
        let m = parse_stream_line(&line);
        assert!(m.is_measured());
        assert_eq!(m.value().map(|s| s.fps), Some(0));
    }

    #[test]
    fn ugen_reported_nan_delivered_is_a_measured_nan_not_a_parse_failure() {
        let line = STREAM_LINE.replace("delivered=0.9995", "delivered=NaN");
        let m = parse_stream_line(&line);
        assert!(m.is_measured(), "NaN is ugen's own encoding of 'no expframes was given', a real value");
        let d = m.value().map(|s| s.delivered).unwrap_or(0.0);
        assert!(d.is_nan());
    }

    #[test]
    fn unknown_extra_stream_fields_are_ignored() {
        let line = format!("{STREAM_LINE} extra=1");
        assert!(parse_stream_line(&line).is_measured());
    }

    // ── gates ────────────────────────────────────────────────────────────────────────────────────

    #[test]
    fn rps_gate_passes_under_the_fail_fraction_and_p99_ceiling() {
        let gate = RpsGate { p99_ceiling_us: 20_000, max_fail_fraction: 0.001 };
        let stats = UgenStats { rps: 1000, fail: 2, p50_us: 5_000, p99_us: 15_000, ok: 11_998 };
        assert!(gate.passed(&stats, 12));
    }

    #[test]
    fn rps_gate_fails_over_the_p99_ceiling_even_with_no_failures() {
        let gate = RpsGate { p99_ceiling_us: 20_000, max_fail_fraction: 0.001 };
        let stats = UgenStats { rps: 1000, fail: 0, p50_us: 5_000, p99_us: 25_000, ok: 12_000 };
        assert!(!gate.passed(&stats, 12));
    }

    #[test]
    fn rps_gate_fails_over_the_error_rate_even_under_the_p99_ceiling() {
        let gate = RpsGate { p99_ceiling_us: 20_000, max_fail_fraction: 0.001 };
        let stats = UgenStats { rps: 100, fail: 50, p50_us: 5_000, p99_us: 10_000, ok: 1_150 };
        assert!(!gate.passed(&stats, 12));
    }

    #[test]
    fn stream_gate_requires_zero_stalled_regardless_of_delivered() {
        let gate = StreamGate { min_delivered: 0.999, max_fail_fraction: 0.001, min_complete_fraction: 0.0 };
        let stats = StreamStats {
            streams: 64,
            complete: 64,
            fail: 0,
            stalled: 1,
            frames: 2048,
            fps: 170,
            delivered: 1.0,
            ttft_p50_us: 1,
            ttft_p99_us: 2,
            gap_p50_us: 1,
            gap_p99_us: 2,
        };
        assert!(!gate.passed(&stats));
    }

    #[test]
    fn stream_gate_nan_delivered_never_passes() {
        let gate = StreamGate { min_delivered: 0.999, max_fail_fraction: 0.001, min_complete_fraction: 0.0 };
        let stats = StreamStats {
            streams: 64,
            complete: 64,
            fail: 0,
            stalled: 0,
            frames: 0,
            fps: 0,
            delivered: f64::NAN,
            ttft_p50_us: 1,
            ttft_p99_us: 2,
            gap_p50_us: 1,
            gap_p99_us: 2,
        };
        assert!(!gate.passed(&stats), "a comparison against NaN must never be treated as satisfying the floor");
    }

    // ── probe budget ─────────────────────────────────────────────────────────────────────────────

    #[test]
    fn probe_budget_adds_the_grace_period() {
        assert_eq!(probe_budget(12), Duration::from_secs(12 + u64::from(PROBE_GRACE_S)));
    }
}
