// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// DURABLE, ATOMIC PUBLICATION OF A RESULT SNAPSHOT.
//
// record.rs describes the shape of the artifact. This module writes it to disk the way a public
// board's only copy of the numbers deserves: never a half-written file at the target path, never a
// rename that vanishes the moment before a self-terminating cloud box dies, and never a worse result
// quietly replacing a better one just because it ran more recently.
//
// Two files come out of one call: the per-gateway CURRENT file (read by whatever renders the board
// today) and a timestamped HISTORICAL copy (read by nothing today, kept so "what did this gateway look
// like on that day" is answerable later). Both live in the same directory the caller hands in; where
// that directory sits relative to results/matrix vs results/snapshots is the caller's concern, not
// this module's.

use crate::record::{Matrix, ResultSnapshot, Served};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The two paths a successful write touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub current: PathBuf,
    pub historical: PathBuf,
}

/// Everything that can keep a snapshot from landing. Every variant carries enough to say WHY without
/// the caller re-deriving it: a bare `io::Error` at the call site cannot say which of the two files
/// (or the directory fsync) it came from, and a promote-guard rejection is not an IO failure at all.
#[derive(Debug)]
pub enum SnapshotError {
    Io { path: PathBuf, source: io::Error },
    Json(serde_json::Error),
    /// The incoming snapshot serves strictly fewer cells than the one already on disk at the current
    /// path. Refusing here is the whole point: a boot failure that served nothing must never overwrite
    /// a prior run that served most of the grid just because it happened to run later.
    PromoteGuard { existing_served: usize, incoming_served: usize },
    /// A value that becomes part of a path is not a safe filename component. Refused rather than
    /// sanitised: silently rewriting a name would publish one gateway's result under another's.
    UnsafeName { what: &'static str, raw: String },
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotError::Io { path, source } => {
                write!(f, "snapshot io error at {}: {source}", path.display())
            }
            SnapshotError::Json(source) => write!(f, "snapshot json error: {source}"),
            SnapshotError::PromoteGuard { existing_served, incoming_served } => write!(
                f,
                "refusing to overwrite a snapshot that served {existing_served} cells with one that served only {incoming_served}"
            ),
            SnapshotError::UnsafeName { what, raw } => write!(
                f,
                "{what} {raw:?} is not a safe filename component, refusing to build a path from it"
            ),
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SnapshotError::Io { source, .. } => Some(source),
            SnapshotError::Json(source) => Some(source),
            SnapshotError::PromoteGuard { .. } | SnapshotError::UnsafeName { .. } => None,
        }
    }
}

/// How many (ingress, egress) cells this matrix actually served. Counted over the full 6x6 grid
/// (`upstreams.*.cells`) when present, since that is the sole numeric source (record.rs's own words);
/// the top-level `cells` map is only a v1-compat mirror of one egress row and would undercount a
/// gateway configured for more than one. Falls back to the compat row only for a matrix that never
/// carried the full grid at all, so an old-shaped snapshot still has a meaningful count.
fn served_cell_count(matrix: &Matrix) -> usize {
    if matrix.upstreams.is_empty() {
        matrix.cells.values().filter(|c| matches!(c.served, Served::Bool(true))).count()
    } else {
        matrix
            .upstreams
            .values()
            .flat_map(|u| u.cells.values())
            .filter(|c| matches!(c.served, Served::Bool(true)))
            .count()
    }
}

/// Write `bytes` to `target` without ever exposing a partial file at that path.
///
/// Temp-then-rename, both fsynced: the temp file is written and fsynced FIRST (a rename only makes the
/// directory entry's name change durable, not the bytes sitting behind it), then renamed over the
/// target (atomic on the same filesystem, which it is: the temp file lives in `dir`, the target's own
/// parent), then the directory itself is fsynced (a rename's directory-entry update is not guaranteed
/// durable on its own, and these runs die on a hard self-termination timer, so "renamed, then the box
/// died a moment later" is a real window, not a theoretical one).
fn atomic_write(dir: &Path, target: &Path, bytes: &[u8]) -> Result<(), SnapshotError> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp_path = dir.join(format!(".snapshot-tmp-{}-{nanos}", std::process::id()));

    let write_result = (|| -> Result<(), SnapshotError> {
        let mut tmp_file = fs::File::create(&tmp_path)
            .map_err(|source| SnapshotError::Io { path: tmp_path.clone(), source })?;
        tmp_file
            .write_all(bytes)
            .map_err(|source| SnapshotError::Io { path: tmp_path.clone(), source })?;
        tmp_file.sync_all().map_err(|source| SnapshotError::Io { path: tmp_path.clone(), source })?;
        Ok(())
    })();

    if let Err(err) = write_result {
        // Best-effort cleanup: nothing at the target path was ever touched, so leaving stray litter
        // behind is a nuisance, not a correctness problem. Its own failure must not mask the real one.
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    fs::rename(&tmp_path, target)
        .map_err(|source| SnapshotError::Io { path: target.to_path_buf(), source })?;

    let dir_handle =
        fs::File::open(dir).map_err(|source| SnapshotError::Io { path: dir.to_path_buf(), source })?;
    dir_handle.sync_all().map_err(|source| SnapshotError::Io { path: dir.to_path_buf(), source })?;

    Ok(())
}

/// Read the existing current-file snapshot at `path`, if any. `Ok(None)` means there is nothing there
/// yet (the ordinary first-run case); any other read or parse failure is reported, never swallowed,
/// because a corrupt current file silently treated as "absent" would let the promote guard wave a
/// worse result straight through.
fn read_existing(path: &Path) -> Result<Option<ResultSnapshot>, SnapshotError> {
    match fs::read(path) {
        Ok(bytes) => {
            let existing: ResultSnapshot = serde_json::from_slice(&bytes).map_err(SnapshotError::Json)?;
            Ok(Some(existing))
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SnapshotError::Io { path: path.to_path_buf(), source }),
    }
}

/// Write `snapshot` durably into `dir`: the per-gateway current file (`<gateway>.json`) and a
/// timestamped historical copy (`result_<gateway>_<measured_at, ':' -> '-'>.json`), both by
/// temp-then-rename-then-fsync (see `atomic_write`).
///
/// If a current file already exists and `snapshot` served strictly fewer cells, this returns
/// `SnapshotError::PromoteGuard` and writes NEITHER file: a rewrite of the promote guard's own
/// rejection into a historical copy would still be publishing the worse result, just under a
/// different name.
/// A filename component the caller supplied, made safe to join onto a directory.
///
/// `PathBuf::join` has a sharp edge: an ABSOLUTE argument replaces the base entirely, and `..`
/// traverses out of it. Both the gateway name and the timestamp reach the path from data, so an
/// unvalidated one could write outside the results tree while the call still reports success and
/// looks like a normal publish in the log. Anything that is not a plain, safe component is rejected
/// rather than sanitised: silently rewriting a name would publish one gateway's result under
/// another's, which is worse than refusing.
fn safe_component(raw: &str, what: &'static str) -> Result<String, SnapshotError> {
    let ok = !raw.is_empty()
        && raw.len() <= 128
        && raw != "."
        && raw != ".."
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !raw.starts_with('.');
    if ok {
        Ok(raw.to_string())
    } else {
        Err(SnapshotError::UnsafeName { what, raw: raw.to_string() })
    }
}

pub fn write_snapshot(dir: &Path, snapshot: &ResultSnapshot) -> Result<Paths, SnapshotError> {
    let incoming_served = served_cell_count(&snapshot.matrix);
    let current_path = dir.join(format!("{}.json", safe_component(&snapshot.gateway, "gateway")?));

    if let Some(existing) = read_existing(&current_path)? {
        let existing_served = served_cell_count(&existing.matrix);
        if incoming_served < existing_served {
            return Err(SnapshotError::PromoteGuard { existing_served, incoming_served });
        }
    }

    // The historical filename's timestamp comes from the snapshot's OWN measured_at, not the clock at
    // write time: re-writing the same measurement later (a retry, a re-publish) must not invent a new
    // measurement instant just because the write happened again.
    let ts_safe = snapshot.measured_at.replace(':', "-");
    let historical_path = dir.join(format!("result_{}_{}.json", safe_component(&snapshot.gateway, "gateway")?, safe_component(&ts_safe, "measured_at")?));

    let mut body =
        serde_json::to_string_pretty(snapshot).map_err(SnapshotError::Json)?;
    body.push('\n');
    let bytes = body.into_bytes();

    atomic_write(dir, &current_path, &bytes)?;
    atomic_write(dir, &historical_path, &bytes)?;

    Ok(Paths { current: current_path, historical: historical_path })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A name that is not a safe path component is REFUSED, not sanitised. PathBuf::join replaces the
    // base entirely when handed an absolute path, and traverses out of it on "..", so an unvalidated
    // gateway name or timestamp could write outside the results tree while the call reported success.
    #[test]
    fn a_name_that_would_escape_the_directory_is_refused() {
        for bad in ["/etc/cron.d/evil", "../../../etc/passwd", "..", ".", "", ".hidden", "a/b", "a\\b"] {
            assert!(
                safe_component(bad, "gateway").is_err(),
                "{bad:?} must not be accepted as a filename component"
            );
        }
    }

    #[test]
    fn ordinary_names_and_timestamps_are_accepted() {
        for good in ["gw", "gw-two", "gw_two", "2026-07-26T00-00-00Z", "a.b"] {
            assert!(safe_component(good, "gateway").is_ok(), "{good:?} should be accepted");
        }
    }

    use crate::record::{Cell, ConfigFiles, Upstream};
    use std::collections::HashMap;

    fn unique_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir()
            .join(format!("otb-engine-snapshot-test-{name}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn served_cell() -> Cell {
        Cell {
            served: Served::Bool(true),
            reason: None,
            status: String::new(),
            path: String::new(),
            verdict_note: String::new(),
            body_snippet: String::new(),
            probe_note: None,
            perf_dropped: None,
            perf: None,
            stream: None,
            memory: None,
            timings_s: None,
        }
    }

    /// A cell that was probed and answered with a status string rather than `true`. The grid always
    /// enumerates every cell (`run::run_grid`, the live walker), so this — not a missing row — is
    /// what a lost capability actually looks like in a snapshot.
    fn unserved_cell(status: &str) -> Cell {
        Cell { served: Served::Status(status.to_string()), ..served_cell() }
    }

    /// `served` cells that answered true, plus `unserved` cells that were probed and did not. Total
    /// cell count is `served + unserved`, so a caller can hold the GRID SIZE fixed and vary only how
    /// many of those cells were actually served.
    fn matrix_of(gateway: &str, measured_at: &str, served: usize, unserved: usize) -> Matrix {
        let mut cells = HashMap::new();
        for i in 0..served {
            cells.insert(format!("ingress{i}"), served_cell());
        }
        for i in 0..unserved {
            cells.insert(format!("unserved{i}"), unserved_cell("not_configured"));
        }
        let mut upstreams = HashMap::new();
        upstreams.insert(
            "eg".to_string(),
            Upstream { configurable: true, served: true, egress_config: None, serve_error: String::new(), cells },
        );
        Matrix {
            gateway: gateway.to_string(),
            build: String::new(),
            matrix_version: Some(2),
            served: true,
            serve_error: String::new(),
            upstream_shape: String::new(),
            upstream_note: String::new(),
            egress_configured: String::new(),
            probe_first: true,
            capability_note: String::new(),
            cell_perf_sweep: false,
            sweep_rung_selection: None,
            sweep_ttft_ms: None,
            p99_ceiling_ms: None,
            sweep_dur: None,
            cell_stream: false,
            cell_memory: None,
            memory: None,
            cells: HashMap::new(),
            upstreams,
            model: None,
            upstream_endpoint: None,
            ootb_config: None,
            arch: None,
            hardware: None,
            rig: None,
            measured_at: measured_at.to_string(),
            started_at: None,
            finished_at: None,
            duration_s: None,
            phase_s: None,
            build_env: None,
        }
    }

    fn snapshot_with_served_cells(gateway: &str, measured_at: &str, n: usize) -> ResultSnapshot {
        snapshot_of(gateway, measured_at, n, 0)
    }

    fn snapshot_of(gateway: &str, measured_at: &str, served: usize, unserved: usize) -> ResultSnapshot {
        ResultSnapshot {
            schema_version: 1,
            gateway: gateway.to_string(),
            build: String::new(),
            measured_at: measured_at.to_string(),
            started_at: None,
            finished_at: None,
            duration_s: None,
            phase_s: None,
            arch: None,
            hardware: None,
            rig: None,
            config: ConfigFiles { files: HashMap::new() },
            matrix: matrix_of(gateway, measured_at, served, unserved),
            memory: None,
            streaming: None,
        }
    }

    // ── round-trip and file placement ───────────────────────────────────────────────────────────

    #[test]
    fn written_snapshot_reads_back_byte_identical_and_equal() {
        let dir = unique_dir("roundtrip");
        let snap = snapshot_with_served_cells("gw", "2026-07-25T08:26:15Z", 2);

        let paths = write_snapshot(&dir, &snap).unwrap();

        let expected = serde_json::to_string_pretty(&snap).unwrap() + "\n";
        let current_text = fs::read_to_string(&paths.current).unwrap();
        assert_eq!(current_text, expected);
        let back: ResultSnapshot = serde_json::from_str(&current_text).unwrap();
        assert_eq!(back, snap);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn both_current_and_historical_files_appear_named_from_measured_at() {
        let dir = unique_dir("both-files");
        let snap = snapshot_with_served_cells("gw", "2026-07-25T08:26:15Z", 1);

        let paths = write_snapshot(&dir, &snap).unwrap();

        assert_eq!(paths.current, dir.join("gw.json"));
        assert_eq!(paths.historical, dir.join("result_gw_2026-07-25T08-26-15Z.json"));
        assert!(paths.current.exists());
        assert!(paths.historical.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn writing_twice_is_idempotent_for_current_and_appends_historical_on_new_timestamp() {
        let dir = unique_dir("twice");
        let first = snapshot_with_served_cells("gw", "2026-07-25T08:26:15Z", 1);
        let second = snapshot_with_served_cells("gw", "2026-07-25T09:00:00Z", 1);

        write_snapshot(&dir, &first).unwrap();
        let paths2 = write_snapshot(&dir, &second).unwrap();

        // current always reflects the latest write.
        let current: ResultSnapshot =
            serde_json::from_str(&fs::read_to_string(&paths2.current).unwrap()).unwrap();
        assert_eq!(current, second);

        // both historical copies exist, one per distinct measured_at.
        assert!(dir.join("result_gw_2026-07-25T08-26-15Z.json").exists());
        assert!(dir.join("result_gw_2026-07-25T09-00-00Z.json").exists());

        let entries: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("result_gw_"))
            .collect();
        assert_eq!(entries.len(), 2, "exactly two historical copies for two distinct measured_at values");

        let _ = fs::remove_dir_all(&dir);
    }

    // ── no partial file at the target path ──────────────────────────────────────────────────────

    #[test]
    fn no_partial_file_left_when_target_directory_is_missing() {
        let parent = unique_dir("missing-parent");
        let missing_dir = parent.join("does-not-exist");
        let snap = snapshot_with_served_cells("gw", "2026-07-25T08:26:15Z", 1);

        let result = write_snapshot(&missing_dir, &snap);

        assert!(result.is_err());
        assert!(!missing_dir.join("gw.json").exists());
        assert!(!missing_dir.exists());

        let _ = fs::remove_dir_all(&parent);
    }

    #[test]
    fn successful_write_leaves_no_temp_file_behind() {
        let dir = unique_dir("no-litter");
        let snap = snapshot_with_served_cells("gw", "2026-07-25T08:26:15Z", 1);

        write_snapshot(&dir, &snap).unwrap();

        let litter: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("snapshot-tmp"))
            .collect();
        assert!(litter.is_empty(), "no temp file should survive a successful write");

        let _ = fs::remove_dir_all(&dir);
    }

    // ── promote guard ────────────────────────────────────────────────────────────────────────────

    #[test]
    fn promote_guard_fires_when_incoming_serves_fewer_cells() {
        let dir = unique_dir("guard-fires");
        let good = snapshot_with_served_cells("gw", "2026-07-25T08:00:00Z", 4);
        let worse = snapshot_with_served_cells("gw", "2026-07-25T09:00:00Z", 1);

        write_snapshot(&dir, &good).unwrap();
        let err = write_snapshot(&dir, &worse).unwrap_err();

        match err {
            SnapshotError::PromoteGuard { existing_served, incoming_served } => {
                assert_eq!(existing_served, 4);
                assert_eq!(incoming_served, 1);
            }
            other => panic!("expected PromoteGuard, got {other:?}"),
        }

        // the good snapshot must still be the one on disk: the guard wrote nothing.
        let current: ResultSnapshot =
            serde_json::from_str(&fs::read_to_string(dir.join("gw.json")).unwrap()).unwrap();
        assert_eq!(current, good);
        assert!(!dir.join("result_gw_2026-07-25T09-00-00Z.json").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    // THE CASE THE GRID ACTUALLY PRODUCES, and the one the two tests around this one cannot see.
    //
    // `run::run_grid` - the LIVE walker, called from suite.rs and bin/otb.rs - enumerates every cell
    // every time, so a real re-run never changes the number of cells: it changes how many of them
    // answered `true`. The neighbouring guard tests vary the cell COUNT (4 vs 1) with every cell
    // served, which a real snapshot pair cannot do, so they cannot exercise `served_cell_count`'s
    // `Served::Bool(true)` filter: this test holds the grid size fixed at 4 and varies only how many
    // of those cells answered true, so a filter dropped in favor of a bare `.count()` would compare
    // 4 against 4 and PROMOTE a run that lost three quarters of its capability.
    #[test]
    fn promote_guard_fires_when_the_grid_is_the_same_size_but_fewer_cells_served() {
        let dir = unique_dir("guard-same-size");
        let good = snapshot_of("gw", "2026-07-25T08:00:00Z", 4, 0);
        // Same four cells, still all present and still all probed: three now answer not_configured.
        let degraded = snapshot_of("gw", "2026-07-25T09:00:00Z", 1, 3);

        assert_eq!(served_cell_count(&good.matrix), 4, "fixture check: the baseline serves four cells");
        assert_eq!(
            good.matrix.upstreams.values().map(|u| u.cells.len()).sum::<usize>(),
            degraded.matrix.upstreams.values().map(|u| u.cells.len()).sum::<usize>(),
            "the two snapshots must describe the SAME grid size, or this test degenerates into the count-based one above"
        );

        write_snapshot(&dir, &good).unwrap();
        let err = write_snapshot(&dir, &degraded).unwrap_err();

        match err {
            SnapshotError::PromoteGuard { existing_served, incoming_served } => {
                assert_eq!(existing_served, 4);
                assert_eq!(incoming_served, 1, "only cells answering true count as served");
            }
            other => panic!("expected PromoteGuard, got {other:?}"),
        }

        let current: ResultSnapshot =
            serde_json::from_str(&fs::read_to_string(dir.join("gw.json")).unwrap()).unwrap();
        assert_eq!(current, good, "the degraded run must not have replaced the good one");

        let _ = fs::remove_dir_all(&dir);
    }

    // The v1-compat branch of `served_cell_count` (the top-level `cells` map, used only by a matrix
    // that never carried the full grid) has the same filter and needs the same hold: every fixture
    // reaching it was all-Bool(true), so replacing its filter with a bare `.count()` was invisible.
    #[test]
    fn the_v1_compat_row_also_counts_only_cells_that_answered_true() {
        let mut m = matrix_of("gw", "2026-07-25T08:00:00Z", 0, 0);
        m.upstreams.clear(); // force the compat branch
        m.cells.insert("openai".into(), served_cell());
        m.cells.insert("anthropic".into(), unserved_cell("not_configured"));
        m.cells.insert("gemini".into(), unserved_cell("not_verified"));

        assert_eq!(m.cells.len(), 3, "fixture check: three cells are present");
        assert_eq!(served_cell_count(&m), 1, "only the cell that answered true is served");
    }

    #[test]
    fn promote_guard_does_not_fire_on_equal_or_better() {
        let dir = unique_dir("guard-passes");
        let baseline = snapshot_with_served_cells("gw", "2026-07-25T08:00:00Z", 2);
        let equal = snapshot_with_served_cells("gw", "2026-07-25T09:00:00Z", 2);
        let better = snapshot_with_served_cells("gw", "2026-07-25T10:00:00Z", 3);

        write_snapshot(&dir, &baseline).unwrap();
        write_snapshot(&dir, &equal).unwrap();
        write_snapshot(&dir, &better).unwrap();

        let current: ResultSnapshot =
            serde_json::from_str(&fs::read_to_string(dir.join("gw.json")).unwrap()).unwrap();
        assert_eq!(current, better);

        let _ = fs::remove_dir_all(&dir);
    }

    // ── unwritable directory ─────────────────────────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn unwritable_directory_returns_error_not_success() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_dir("unwritable");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();

        let snap = snapshot_with_served_cells("gw", "2026-07-25T08:26:15Z", 1);
        let result = write_snapshot(&dir, &snap);

        // restore write permission before cleanup, regardless of the assertion outcome below.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err(), "a read-only directory must not report a successful write");
        assert!(!dir.join("gw.json").exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
