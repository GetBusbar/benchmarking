use super::*;

/// How many (ingress, egress) cells this matrix actually served. Counted over the full 6x6 grid
/// (`upstreams.*.cells`) when present, since that is the sole numeric source (record.rs's own words);
/// the top-level `cells` map is only a v1-compat mirror of one egress row and would undercount a
/// gateway configured for more than one. Falls back to the compat row only for a matrix that never
/// carried the full grid at all, so an old-shaped snapshot still has a meaningful count.
fn served_cell_count(matrix: &Matrix) -> usize {
    served_cell_keys(matrix).len()
}

// A name that is not a safe path component is REFUSED, not sanitised. PathBuf::join replaces the
// base entirely when handed an absolute path, and traverses out of it on "..", so an unvalidated
// gateway name or timestamp could write outside the results tree while the call reported success.
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

/// A cell that was probed and answered with a status string rather than `true`. The grid always
/// enumerates every cell (`run::run_grid`, the live walker), so this — not a missing row — is
/// what a lost capability actually looks like in a snapshot.
fn unserved_cell(status: &str) -> Cell {
    Cell {
        served: Served::Status(status.to_string()),
        ..served_cell()
    }
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

fn snapshot_of(gateway: &str, measured_at: &str, served: usize, unserved: usize) -> ResultSnapshot {
    ResultSnapshot {
        schema_version: 1,
        gateway_provenance: None,
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

// THE PAIR IS NOT ATOMIC AS A PAIR, SO THE ORDER IS THE GUARANTEE.
//
// Each file is written atomically, but two atomic writes are not one transaction: a box that dies
// between them - which these self-terminating runs really do - leaves one of the two behind. With
// the current file written first, that state was "a promoted result the board reads, with no
// historical copy behind it", so the day's numbers existed with nothing able to answer "what did
// this gateway look like then" and nothing recording that a copy was missing. Historical first
// inverts it into the harmless half: a copy on disk, simply not promoted yet, which the next run
// rewrites in full.
//
// Observed from the side the test can drive: make the HISTORICAL write fail (a directory sitting
// at its path cannot be renamed over) and assert nothing was promoted. Under the old order the
// current file was already on disk by then - the board reading a result with no history behind it,
// and no error anywhere the board could see.
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

// The v1-compat branch of `served_cell_count` (the top-level `cells` map, used only by a matrix
// that never carried the full grid) has the same filter and needs the same hold: every fixture
// reaching it was all-Bool(true), so replacing its filter with a bare `.count()` was invisible.
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

// The guard only compares the AGGREGATE served count, so a run that loses a previously-measured
// cell while gaining a different one slips through with `incoming_served >= existing_served` and
// silently overwrites the good measurement for the lost cell. Same grid size (two cells, "a" and
// "b"), same served count (1) both times, but WHICH cell answered true flips between writes.
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
    // AS ROOT a read-only mode does not stop a write (CAP_DAC_OVERRIDE), so the premise of this test
    // does not hold there and its failure would say nothing about the code. Probe the premise first.
    let probe = dir.join(".probe");
    if fs::write(&probe, b"x").is_ok() {
        let _ = fs::remove_file(&probe);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let _ = fs::remove_dir_all(&dir);
        eprintln!("skipping unwritable_directory_returns_error_not_success: mode 0o555 does not stop writes here (running as root?)");
        return;
    }

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

// A RENAME THAT FAILS MUST NOT LEAVE THE SNAPSHOT BEHIND AT A HIDDEN PATH. Only a failed write used to
// clean up; a failed rename left a full copy as `.snapshot-tmp-*` in the results directory.
#[test]
fn a_failed_rename_leaves_no_temp_file_behind() {
    let dir = unique_dir("rename-fails");
    let snap = snapshot_with_served_cells("gw", "2026-07-25T08:26:15Z", 1);
    // The historical copy is written first; a non-empty DIRECTORY at its path makes the rename fail.
    let blocker = dir.join("result_gw_2026-07-25T08-26-15Z.json");
    fs::create_dir_all(blocker.join("occupied")).unwrap();

    let result = write_snapshot(&dir, &snap);

    let leftovers: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".snapshot-tmp-"))
        .collect();
    let _ = fs::remove_dir_all(&dir);
    assert!(result.is_err(), "the rename failed, so the write did");
    assert!(
        leftovers.is_empty(),
        "temp files left behind: {leftovers:?}"
    );
}

/// The same snapshot in the v1-compat shape: its served cells in the top-level `cells` row, no grid.
fn v1_snapshot_of(gateway: &str, measured_at: &str, served: usize) -> ResultSnapshot {
    let mut snap = snapshot_of(gateway, measured_at, served, 0);
    let grid = std::mem::take(&mut snap.matrix.upstreams);
    for upstream in grid.into_values() {
        snap.matrix.cells.extend(upstream.cells);
    }
    snap
}

// ACROSS A SHAPE CHANGE there is no shared cell identity (the v1 row is keyed by ingress alone), so
// the guard falls back to the aggregate count. Per-cell comparison there would read a legitimate v1
// file on disk as losing every cell and wedge promotion for that directory forever.
#[test]
fn a_shape_change_is_judged_by_count_not_by_cell_identity() {
    let dir = unique_dir("shape-change");
    write_snapshot(&dir, &v1_snapshot_of("gw", "2026-07-25T08:00:00Z", 3)).expect("seed v1");

    // Same count, new shape: promotes, although no key matches.
    let same = snapshot_with_served_cells("gw", "2026-07-25T09:00:00Z", 3);
    assert!(
        write_snapshot(&dir, &same).is_ok(),
        "a v2 run serving as many cells as the v1 file must promote"
    );

    // Back to v1 on disk, then a v2 run serving fewer: refused.
    let dir2 = unique_dir("shape-change-fewer");
    write_snapshot(&dir2, &v1_snapshot_of("gw", "2026-07-25T08:00:00Z", 3)).expect("seed v1");
    let fewer = snapshot_with_served_cells("gw", "2026-07-25T09:00:00Z", 2);
    let refused = write_snapshot(&dir2, &fewer);
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&dir2);
    match refused {
        Err(SnapshotError::PromoteGuard {
            existing_served: 3,
            incoming_served: 2,
        }) => {}
        other => panic!("expected the count rule to refuse 3 -> 2, got {other:?}"),
    }
}
