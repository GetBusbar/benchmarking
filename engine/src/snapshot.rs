// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// Durable, atomic publication of a result snapshot (record.rs describes the artifact's shape). Never
// a half-written file, never a rename lost to a self-terminating box dying mid-write, never a worse
// result silently replacing a better one just because it ran more recently.
//
// One call writes two files: the per-gateway CURRENT file (what the board renders today) and a
// timestamped HISTORICAL copy (kept for "what did this gateway look like on that day"). Both live in
// the directory the caller passes in.

use crate::record::{Matrix, ResultSnapshot, Served};
use std::collections::BTreeSet;
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
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Json(serde_json::Error),
    /// The incoming snapshot serves strictly fewer cells than the one already on disk at the current
    /// path. Refusing here is the whole point: a boot failure that served nothing must never overwrite
    /// a prior run that served most of the grid just because it happened to run later.
    PromoteGuard {
        existing_served: usize,
        incoming_served: usize,
    },
    /// A value that becomes part of a path is not a safe filename component. Refused rather than
    /// sanitised: silently rewriting a name would publish one gateway's result under another's.
    UnsafeName {
        what: &'static str,
        raw: String,
    },
    /// A header this gateway's manifest declares could not be resolved. Refused rather than run with
    /// no headers, since that would make every cell fail to serve and a reader would blame the
    /// gateway rather than the harness's dropped auth.
    UnresolvableHeader {
        detail: String,
    },
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
            SnapshotError::UnresolvableHeader { detail } => write!(
                f,
                "a header this manifest declares could not be resolved ({detail}) - refusing to \
                 measure with no headers, because every cell would fail to serve and the board would \
                 report the GATEWAY as not serving when the harness dropped its headers"
            ),
        }
    }
}

impl std::error::Error for SnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SnapshotError::Io { source, .. } => Some(source),
            SnapshotError::Json(source) => Some(source),
            SnapshotError::PromoteGuard { .. }
            | SnapshotError::UnsafeName { .. }
            | SnapshotError::UnresolvableHeader { .. } => None,
        }
    }
}

/// How many (egress, ingress) cells this matrix actually served. Uses the full grid
/// (`upstreams.*.cells`) when present; falls back to the top-level `cells` map only for an old-shaped
/// snapshot that never carried the full grid (that map would otherwise undercount).
#[cfg(test)]
fn served_cell_count(matrix: &Matrix) -> usize {
    served_cell_keys(matrix).len()
}

/// The identity of every cell this matrix actually served, as (egress, ingress) keys. The v1-compat
/// top-level `cells` row has no egress dimension, so it's keyed by an empty egress — which is why the
/// two branches' keys are never compared against each other (see `comparable` in `write_snapshot`).
fn served_cell_keys(matrix: &Matrix) -> BTreeSet<(String, String)> {
    if matrix.upstreams.is_empty() {
        matrix
            .cells
            .iter()
            .filter(|(_, c)| matches!(c.served, Served::Bool(true)))
            .map(|(ingress, _)| (String::new(), ingress.clone()))
            .collect()
    } else {
        matrix
            .upstreams
            .iter()
            .flat_map(|(egress, u)| {
                u.cells
                    .iter()
                    .filter(|(_, c)| matches!(c.served, Served::Bool(true)))
                    .map(move |(ingress, _)| (egress.clone(), ingress.clone()))
            })
            .collect()
    }
}

/// Write `bytes` to `target` without ever exposing a partial file at that path.
///
/// Temp file written and fsynced first, then renamed over the target (atomic — same filesystem), then
/// the directory itself fsynced (a rename's directory-entry update isn't durable on its own, and these
/// runs die on a hard self-termination timer, so that window is real).
fn atomic_write(dir: &Path, target: &Path, bytes: &[u8]) -> Result<(), SnapshotError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_path = dir.join(format!(".snapshot-tmp-{}-{nanos}", std::process::id()));

    let write_result = (|| -> Result<(), SnapshotError> {
        let mut tmp_file = fs::File::create(&tmp_path).map_err(|source| SnapshotError::Io {
            path: tmp_path.clone(),
            source,
        })?;
        tmp_file
            .write_all(bytes)
            .map_err(|source| SnapshotError::Io {
                path: tmp_path.clone(),
                source,
            })?;
        tmp_file.sync_all().map_err(|source| SnapshotError::Io {
            path: tmp_path.clone(),
            source,
        })?;
        Ok(())
    })();

    if let Err(err) = write_result {
        // Best-effort cleanup: nothing at the target path was ever touched, so leaving stray litter
        // behind is a nuisance, not a correctness problem. Its own failure must not mask the real one.
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    fs::rename(&tmp_path, target).map_err(|source| SnapshotError::Io {
        path: target.to_path_buf(),
        source,
    })?;

    let dir_handle = fs::File::open(dir).map_err(|source| SnapshotError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    dir_handle.sync_all().map_err(|source| SnapshotError::Io {
        path: dir.to_path_buf(),
        source,
    })?;

    Ok(())
}

/// Read the existing current-file snapshot at `path`, if any. `Ok(None)` means genuinely absent
/// (first run); other read/parse failures are reported rather than treated as absent, since that
/// would let the promote guard wave a worse result through.
fn read_existing(path: &Path) -> Result<Option<ResultSnapshot>, SnapshotError> {
    match fs::read(path) {
        Ok(bytes) => {
            let existing: ResultSnapshot =
                serde_json::from_slice(&bytes).map_err(SnapshotError::Json)?;
            Ok(Some(existing))
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SnapshotError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// A filename component the caller supplied, made safe to join onto a directory.
///
/// `PathBuf::join` replaces the base entirely on an absolute argument and `..` traverses out of it,
/// so an unvalidated gateway name or timestamp could write outside the results tree while reporting
/// success. Rejected rather than sanitised: silently rewriting a name could publish one gateway's
/// result under another's.
fn safe_component(raw: &str, what: &'static str) -> Result<String, SnapshotError> {
    let ok = !raw.is_empty()
        && raw.len() <= 128
        && raw != "."
        && raw != ".."
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !raw.starts_with('.');
    if ok {
        Ok(raw.to_string())
    } else {
        Err(SnapshotError::UnsafeName {
            what,
            raw: raw.to_string(),
        })
    }
}

/// Write `snapshot` durably into `dir`: the per-gateway current file (`<gateway>.json`) and a
/// timestamped historical copy (`result_<gateway>_<measured_at, ':' -> '-'>.json`), both via
/// `atomic_write`.
///
/// If a current file already exists and `snapshot` served strictly fewer cells, returns
/// `SnapshotError::PromoteGuard` and writes neither file.
///
/// Each file is atomic; the pair is not. So order is the guarantee: historical is written first,
/// current second. A box dying between the two renames then leaves "historical with no current" (a
/// result on disk, simply not yet promoted, rewritten by the next run) rather than "current with no
/// historical" (a promoted, board-visible result with no history behind it and no record it's
/// missing).
pub fn write_snapshot(dir: &Path, snapshot: &ResultSnapshot) -> Result<Paths, SnapshotError> {
    let current_path = dir.join(format!(
        "{}.json",
        safe_component(&snapshot.gateway, "gateway")?
    ));

    if let Some(existing) = read_existing(&current_path)? {
        let existing_keys = served_cell_keys(&existing.matrix);
        let incoming_keys = served_cell_keys(&snapshot.matrix);

        // Per-cell keys are only comparable within the same branch (grid vs v1-compat row); a shape
        // mismatch would read a legitimate v1-on-disk/v2-incoming pair as losing every cell, so it
        // falls back to the aggregate-count rule instead.
        let comparable =
            existing.matrix.upstreams.is_empty() == snapshot.matrix.upstreams.is_empty();
        let regressed = if comparable {
            !existing_keys.is_subset(&incoming_keys)
        } else {
            incoming_keys.len() < existing_keys.len()
        };
        if regressed {
            return Err(SnapshotError::PromoteGuard {
                existing_served: existing_keys.len(),
                incoming_served: incoming_keys.len(),
            });
        }
    }

    // Timestamp comes from the snapshot's own measured_at, not the write-time clock, so a retry or
    // re-publish doesn't invent a new measurement instant.
    let ts_safe = snapshot.measured_at.replace(':', "-");
    let historical_path = dir.join(format!(
        "result_{}_{}.json",
        safe_component(&snapshot.gateway, "gateway")?,
        safe_component(&ts_safe, "measured_at")?
    ));

    let mut body = serde_json::to_string_pretty(snapshot).map_err(SnapshotError::Json)?;
    body.push('\n');
    let bytes = body.into_bytes();

    // Historical first, current second — see the function doc comment for why the order matters.
    atomic_write(dir, &historical_path, &bytes)?;
    atomic_write(dir, &current_path, &bytes)?;

    Ok(Paths {
        current: current_path,
        historical: historical_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_that_would_escape_the_directory_is_refused() {
        for bad in [
            "/etc/cron.d/evil",
            "../../../etc/passwd",
            "..",
            ".",
            "",
            ".hidden",
            "a/b",
            "a\\b",
        ] {
            assert!(
                safe_component(bad, "gateway").is_err(),
                "{bad:?} must not be accepted as a filename component"
            );
        }
    }

    #[test]
    fn ordinary_names_and_timestamps_are_accepted() {
        for good in ["gw", "gw-two", "gw_two", "2026-07-26T00-00-00Z", "a.b"] {
            assert!(
                safe_component(good, "gateway").is_ok(),
                "{good:?} should be accepted"
            );
        }
    }

    use crate::record::{Cell, ConfigFiles, Upstream};
    use std::collections::HashMap;

    fn unique_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "otb-engine-snapshot-test-{name}-{}-{nanos}",
            std::process::id()
        ));
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

    /// A cell that was probed and answered with a status string rather than `true`. This — not a
    /// missing row, since the grid always enumerates every cell — is what a lost capability looks
    /// like in a snapshot.
    fn unserved_cell(status: &str) -> Cell {
        Cell {
            served: Served::Status(status.to_string()),
            ..served_cell()
        }
    }

    /// `served` cells that answered true, plus `unserved` cells that were probed and did not. Total
    /// cell count is `served + unserved`, so a caller can hold grid size fixed and vary only how many
    /// answered true.
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
            Upstream {
                configurable: true,
                served: true,
                egress_config: None,
                serve_error: String::new(),
                cells,
            },
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

    fn snapshot_of(
        gateway: &str,
        measured_at: &str,
        served: usize,
        unserved: usize,
    ) -> ResultSnapshot {
        ResultSnapshot {
            schema_version: 1,
            definitions: Default::default(),
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
            config: ConfigFiles {
                files: HashMap::new(),
            },
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
        assert_eq!(
            paths.historical,
            dir.join("result_gw_2026-07-25T08-26-15Z.json")
        );
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
        assert_eq!(
            entries.len(),
            2,
            "exactly two historical copies for two distinct measured_at values"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    // Regression test for the write-order guarantee (see write_snapshot's doc comment): forces the
    // historical write to fail (a directory sitting at its path can't be renamed over) and asserts
    // nothing was promoted to current.
    #[test]
    fn a_result_is_never_promoted_before_its_historical_copy_is_durable() {
        let dir = unique_dir("pair-order");
        let snap = snapshot_with_served_cells("gw", "2026-07-25T08:26:15Z", 1);
        fs::create_dir_all(dir.join("result_gw_2026-07-25T08-26-15Z.json")).unwrap();

        let result = write_snapshot(&dir, &snap);

        assert!(
            result.is_err(),
            "a historical copy that could not be written must be reported, not swallowed"
        );
        assert!(
            !dir.join("gw.json").exists(),
            "nothing may be promoted to the current file while its historical copy is missing"
        );

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
        assert!(
            litter.is_empty(),
            "no temp file should survive a successful write"
        );

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
            SnapshotError::PromoteGuard {
                existing_served,
                incoming_served,
            } => {
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

    // The case a real re-run actually produces: `run::run_grid` enumerates every cell every time, so
    // grid size never changes between writes, only how many cells answer `true`. This test holds grid
    // size fixed at 4 (unlike the neighboring tests, which vary cell count) so it actually exercises
    // `served_cell_count`'s `Served::Bool(true)` filter — dropping that filter for a bare `.count()`
    // would compare 4 against 4 here and wrongly promote.
    #[test]
    fn promote_guard_fires_when_the_grid_is_the_same_size_but_fewer_cells_served() {
        let dir = unique_dir("guard-same-size");
        let good = snapshot_of("gw", "2026-07-25T08:00:00Z", 4, 0);
        // Same four cells, still all present and still all probed: three now answer not_configured.
        let degraded = snapshot_of("gw", "2026-07-25T09:00:00Z", 1, 3);

        assert_eq!(
            served_cell_count(&good.matrix),
            4,
            "fixture check: the baseline serves four cells"
        );
        assert_eq!(
            good.matrix.upstreams.values().map(|u| u.cells.len()).sum::<usize>(),
            degraded.matrix.upstreams.values().map(|u| u.cells.len()).sum::<usize>(),
            "the two snapshots must describe the SAME grid size, or this test degenerates into the count-based one above"
        );

        write_snapshot(&dir, &good).unwrap();
        let err = write_snapshot(&dir, &degraded).unwrap_err();

        match err {
            SnapshotError::PromoteGuard {
                existing_served,
                incoming_served,
            } => {
                assert_eq!(existing_served, 4);
                assert_eq!(
                    incoming_served, 1,
                    "only cells answering true count as served"
                );
            }
            other => panic!("expected PromoteGuard, got {other:?}"),
        }

        let current: ResultSnapshot =
            serde_json::from_str(&fs::read_to_string(dir.join("gw.json")).unwrap()).unwrap();
        assert_eq!(
            current, good,
            "the degraded run must not have replaced the good one"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    // The v1-compat branch (top-level `cells` map) has the same filter and needs the same coverage:
    // prior fixtures reaching it were all-Bool(true), so a bare `.count()` would have been invisible.
    #[test]
    fn the_v1_compat_row_also_counts_only_cells_that_answered_true() {
        let mut m = matrix_of("gw", "2026-07-25T08:00:00Z", 0, 0);
        m.upstreams.clear(); // force the compat branch
        m.cells.insert("openai".into(), served_cell());
        m.cells
            .insert("anthropic".into(), unserved_cell("not_configured"));
        m.cells
            .insert("gemini".into(), unserved_cell("not_verified"));

        assert_eq!(m.cells.len(), 3, "fixture check: three cells are present");
        assert_eq!(
            served_cell_count(&m),
            1,
            "only the cell that answered true is served"
        );
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

    // A run that loses one previously-served cell while gaining a different one keeps the aggregate
    // count unchanged; the guard must still catch it via per-cell comparison. Same two cells ("a",
    // "b"), same served count (1), but which one answered true flips between writes.
    #[test]
    fn promote_guard_fires_when_a_previously_served_cell_is_lost_even_if_another_is_gained() {
        let dir = unique_dir("guard-per-cell");

        let mut existing_cells = HashMap::new();
        existing_cells.insert("a".to_string(), served_cell());
        existing_cells.insert("b".to_string(), unserved_cell("not_configured"));
        let mut existing = snapshot_of("gw", "2026-07-25T08:00:00Z", 0, 0);
        existing.matrix.upstreams.insert(
            "eg".to_string(),
            Upstream {
                configurable: true,
                served: true,
                egress_config: None,
                serve_error: String::new(),
                cells: existing_cells,
            },
        );

        let mut incoming_cells = HashMap::new();
        incoming_cells.insert("a".to_string(), unserved_cell("not_configured"));
        incoming_cells.insert("b".to_string(), served_cell());
        let mut incoming = snapshot_of("gw", "2026-07-25T09:00:00Z", 0, 0);
        incoming.matrix.upstreams.insert(
            "eg".to_string(),
            Upstream {
                configurable: true,
                served: true,
                egress_config: None,
                serve_error: String::new(),
                cells: incoming_cells,
            },
        );

        assert_eq!(served_cell_count(&existing.matrix), 1);
        assert_eq!(served_cell_count(&incoming.matrix), 1);

        write_snapshot(&dir, &existing).unwrap();
        let result = write_snapshot(&dir, &incoming);

        assert!(
            result.is_err(),
            "losing a previously-served cell (\"a\") must be refused even though a different \
             cell (\"b\") was gained and the aggregate count is unchanged"
        );

        let current: ResultSnapshot =
            serde_json::from_str(&fs::read_to_string(dir.join("gw.json")).unwrap()).unwrap();
        assert_eq!(
            current, existing,
            "the current file must still show cell \"a\" as served, not have been overwritten \
             by a run that lost it"
        );

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

        assert!(
            result.is_err(),
            "a read-only directory must not report a successful write"
        );
        assert!(!dir.join("gw.json").exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
