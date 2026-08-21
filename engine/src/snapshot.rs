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
    /// A header this gateway's manifest declares could not be resolved, so the run would have measured
    /// it with NO headers at all.
    ///
    /// Refused rather than tolerated, because the alternative is the worst outcome this harness can
    /// produce: every cell fails to serve, the board publishes `served: false`, and a reader concludes
    /// the GATEWAY does not work when in fact the harness dropped its authentication. `validate()`
    /// checks headers for shell-style `$` but never exercises `{...}` substitution, so a manifest with
    /// an unknown placeholder passes the gate and reaches the measurement path.
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

/// How many (ingress, egress) cells this matrix actually served. Counted over the full 6x6 grid
/// (`upstreams.*.cells`) when present, since that is the sole numeric source (record.rs's own words);
/// the top-level `cells` map is only a v1-compat mirror of one egress row and would undercount a
/// gateway configured for more than one. Falls back to the compat row only for a matrix that never
/// carried the full grid at all, so an old-shaped snapshot still has a meaningful count.
#[cfg(test)]
fn served_cell_count(matrix: &Matrix) -> usize {
    served_cell_keys(matrix).len()
}

/// The identity of every cell this matrix actually served, as (egress, ingress) keys. Grid cells
/// (`upstreams.*.cells`) are keyed by their real egress and ingress; the v1-compat top-level `cells`
/// row has no egress dimension of its own, so it is keyed by an empty egress alongside the ingress -
/// which is also why keys from the two branches are never compared against each other (see the
/// `comparable` check in `write_snapshot`).
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
/// Temp-then-rename, both fsynced: the temp file is written and fsynced FIRST (a rename only makes the
/// directory entry's name change durable, not the bytes sitting behind it), then renamed over the
/// target (atomic on the same filesystem, which it is: the temp file lives in `dir`, the target's own
/// parent), then the directory itself is fsynced (a rename's directory-entry update is not guaranteed
/// durable on its own, and these runs die on a hard self-termination timer, so "renamed, then the box
/// died a moment later" is a real window, not a theoretical one).
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

/// Read the existing current-file snapshot at `path`, if any. `Ok(None)` means there is nothing there
/// yet (the ordinary first-run case); any other read or parse failure is reported, never swallowed,
/// because a corrupt current file silently treated as "absent" would let the promote guard wave a
/// worse result straight through.
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
/// timestamped historical copy (`result_<gateway>_<measured_at, ':' -> '-'>.json`), both by
/// temp-then-rename-then-fsync (see `atomic_write`).
///
/// If a current file already exists and `snapshot` served strictly fewer cells, this returns
/// `SnapshotError::PromoteGuard` and writes NEITHER file: a rewrite of the promote guard's own
/// rejection into a historical copy would still be publishing the worse result, just under a
/// different name.
///
/// EACH FILE IS ATOMIC; THE PAIR IS NOT, and it cannot be without a two-phase commit this module has
/// no reason to grow. So the ORDER is the guarantee instead: the historical copy lands first, the
/// current file second. That makes the one observable in-between state - a box that dies between the
/// two renames, which these self-terminating runs really do - "a historical copy with no current
/// file", which is a run whose result is on disk and simply not promoted yet, and which the next run
/// rewrites in full. The other order produced the opposite state: a promoted current file, read by
/// the board, with no historical copy behind it, so the day's number existed with nothing to answer
/// "what did this gateway look like then" and nothing anywhere recording that a copy was missing.
///
/// A failure on the SECOND write is therefore reported with both files' fate implied by the error's
/// path: the historical copy is already durable, the current file is not, and the caller sees an
/// `Io` error naming the current path.
pub fn write_snapshot(dir: &Path, snapshot: &ResultSnapshot) -> Result<Paths, SnapshotError> {
    let current_path = dir.join(format!(
        "{}.json",
        safe_component(&snapshot.gateway, "gateway")?
    ));

    if let Some(existing) = read_existing(&current_path)? {
        let existing_keys = served_cell_keys(&existing.matrix);
        let incoming_keys = served_cell_keys(&snapshot.matrix);

        // Cell identity is only comparable when both snapshots come from the same branch of the
        // served-cell walk: the grid keys by (egress, ingress), the v1-compat row by ingress alone
        // (empty egress). Comparing keys across that shape boundary would read a legitimate v1 file
        // on disk / v2 run incoming as losing every cell and wedge promotion for that directory
        // forever, so a shape mismatch keeps the old aggregate-count rule instead.
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

    // The historical filename's timestamp comes from the snapshot's OWN measured_at, not the clock at
    // write time: re-writing the same measurement later (a retry, a re-publish) must not invent a new
    // measurement instant just because the write happened again.
    let ts_safe = snapshot.measured_at.replace(':', "-");
    let historical_path = dir.join(format!(
        "result_{}_{}.json",
        safe_component(&snapshot.gateway, "gateway")?,
        safe_component(&ts_safe, "measured_at")?
    ));

    let mut body = serde_json::to_string_pretty(snapshot).map_err(SnapshotError::Json)?;
    body.push('\n');
    let bytes = body.into_bytes();

    // HISTORICAL FIRST, CURRENT SECOND. See this function's header: the pair is not atomic as a pair,
    // so the order chooses which half-written state a crash can leave behind.
    atomic_write(dir, &historical_path, &bytes)?;
    atomic_write(dir, &current_path, &bytes)?;

    Ok(Paths {
        current: current_path,
        historical: historical_path,
    })
}

/// Why a set of shard snapshots could not be merged into one board row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeError {
    /// No shards were given.
    Empty,
    /// A field that MUST be identical across shards — the proof they measured the same gateway build
    /// on the same instrument — differed. Named so the operator sees WHAT disagreed.
    Mismatch { field: &'static str },
    /// Two shards both measured the same egress column. Shards must own DISJOINT columns; an overlap
    /// means the shard plan double-counted, and a silent union would drop one box's numbers.
    OverlappingEgress { egress: String },
}

impl std::fmt::Display for MergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MergeError::Empty => write!(f, "no shards to merge"),
            MergeError::Mismatch { field } => write!(
                f,
                "shards disagree on `{field}` - they are not the same experiment and must not share a row"
            ),
            MergeError::OverlappingEgress { egress } => write!(
                f,
                "two shards both measured egress `{egress}` - shards must own disjoint egress columns"
            ),
        }
    }
}
impl std::error::Error for MergeError {}

/// Sum a set of `Option<i64>`, yielding `None` only when every input was `None` (so a merged run with
/// no timing recorded stays absent rather than reporting a spurious 0).
fn sum_opt_i64(it: impl Iterator<Item = Option<i64>>) -> Option<i64> {
    let mut acc: Option<i64> = None;
    for v in it.flatten() {
        acc = Some(acc.unwrap_or(0) + v);
    }
    acc
}

fn sum_phase(shards: &[ResultSnapshot]) -> Option<crate::record::PhaseSeconds> {
    let phases: Vec<_> = shards.iter().filter_map(|s| s.phase_s.clone()).collect();
    if phases.is_empty() {
        return None;
    }
    Some(crate::record::PhaseSeconds {
        build: sum_opt_i64(phases.iter().map(|p| p.build)),
        matrix_6x6: sum_opt_i64(phases.iter().map(|p| p.matrix_6x6)),
        memory_window: sum_opt_i64(phases.iter().map(|p| p.memory_window)),
    })
}

/// Merge N per-shard snapshots — each carrying a DISJOINT subset of `matrix.upstreams` (one or more
/// egress columns measured on its own box) — into ONE snapshot shaped exactly like a single-box run.
///
/// INVARIANTS (identical across every shard, else the merge refuses): `schema_version`, `gateway`,
/// `build`, `arch`, rendered `config`, metric `definitions`, the rig's engine/mock/release identity,
/// and `matrix.gateway`/`matrix.build`. A violated invariant means the shards did not measure the
/// same gateway build on the same instrument, so they may not share a board row.
///
/// Each shard's OWN box-qualification (`rig.box_qualify`) and `hardware` are per-column — recorded
/// onto the `Upstream` blocks it contributes, so the merged row keeps every box's qualification as
/// evidence. Snapshot-level provenance (`hardware`, `rig`) stays the first shard's, as the canonical
/// merge-time value; per-column truth lives on the upstreams.
///
/// Publish ONLY the merged result through `write_snapshot`. Never write an individual single-column
/// shard into the canonical dir — it would trip the promote guard against a fuller prior.
pub fn merge_snapshots(shards: &[ResultSnapshot]) -> Result<ResultSnapshot, MergeError> {
    let (first, rest) = shards.split_first().ok_or(MergeError::Empty)?;

    // rig IDENTITY = engine + mock + release_url; box_qualify and ugen are per-box and excluded.
    let rig_identity = |s: &ResultSnapshot| {
        s.rig
            .as_ref()
            .map(|r| (r.engine.clone(), r.mock.clone(), r.release_url.clone()))
    };
    for s in rest {
        let bad = if s.schema_version != first.schema_version {
            Some("schema_version")
        } else if s.gateway != first.gateway {
            Some("gateway")
        } else if s.build != first.build {
            Some("build")
        } else if s.arch != first.arch {
            Some("arch")
        } else if s.config != first.config {
            Some("config")
        } else if s.definitions != first.definitions {
            Some("definitions")
        } else if rig_identity(s) != rig_identity(first) {
            Some("rig")
        } else if s.matrix.gateway != first.matrix.gateway {
            Some("matrix.gateway")
        } else if s.matrix.build != first.matrix.build {
            Some("matrix.build")
        } else {
            None
        };
        if let Some(field) = bad {
            return Err(MergeError::Mismatch { field });
        }
    }

    // Union the egress columns — disjoint — stamping each with the box that measured it.
    let mut upstreams: std::collections::HashMap<String, crate::record::Upstream> = Default::default();
    for s in shards {
        let bq = s.rig.as_ref().and_then(|r| r.box_qualify.clone());
        for (egress, up) in &s.matrix.upstreams {
            let mut up = up.clone();
            up.box_qualify = bq.clone();
            up.hardware = s.hardware.clone();
            if upstreams.insert(egress.clone(), up).is_some() {
                return Err(MergeError::OverlappingEgress {
                    egress: egress.clone(),
                });
            }
        }
    }

    // Combine the per-shard scalars. measured_at names the historical file, so the canonical (min)
    // wins; started/finished bracket the whole sharded run; duration/phase sum the total box-work.
    let measured_at = shards
        .iter()
        .map(|s| s.measured_at.clone())
        .min()
        .unwrap_or_default();
    let started_at = shards.iter().filter_map(|s| s.started_at.clone()).min();
    let finished_at = shards.iter().filter_map(|s| s.finished_at.clone()).max();
    let duration_s = sum_opt_i64(shards.iter().map(|s| s.duration_s));
    let phase_s = sum_phase(shards);
    let served = shards.iter().any(|s| s.matrix.served);
    // The best-diagonal streaming projection is a quick-glance convenience; each shard already
    // projects its own diagonal, and the cell it references is present in the merged matrix. Carry
    // the first shard that produced one.
    let streaming = shards.iter().find_map(|s| s.streaming.clone());

    let mut merged = first.clone();
    merged.measured_at = measured_at.clone();
    merged.started_at = started_at.clone();
    merged.finished_at = finished_at.clone();
    merged.duration_s = duration_s;
    merged.phase_s = phase_s.clone();
    merged.streaming = streaming;
    merged.matrix.upstreams = upstreams;
    merged.matrix.served = served;
    merged.matrix.measured_at = measured_at;
    merged.matrix.started_at = started_at;
    merged.matrix.finished_at = finished_at;
    merged.matrix.duration_s = duration_s;
    merged.matrix.phase_s = phase_s;
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ── merge_snapshots ───────────────────────────────────────────────────────────────────────
    // A shard snapshot: one egress column measured on its own box, with box-specific hardware and
    // qualification. Built off `snapshot_of` (one served cell), re-keyed to the chosen egress, with
    // the invariant fields (build/arch/config/rig identity) held identical across shards so the
    // ONLY thing under test is the union + per-column provenance.
    fn shard(
        gateway: &str,
        egress: &str,
        hardware: &str,
        qualify: serde_json::Value,
        measured_at: &str,
    ) -> ResultSnapshot {
        let mut s = snapshot_of(gateway, measured_at, 1, 0);
        let up = s
            .matrix
            .upstreams
            .remove("eg")
            .expect("snapshot_of keys one upstream under 'eg'");
        s.matrix.upstreams.insert(egress.to_string(), up);
        s.schema_version = 2;
        s.build = "img:1".to_string();
        s.matrix.build = "img:1".to_string();
        s.arch = Some("arm64".to_string());
        s.hardware = Some(hardware.to_string());
        s.rig = Some(crate::record::RigProvenance {
            arch: Some("arm64".to_string()),
            release_url: Some("rig-url".to_string()),
            engine: None,
            mock: None,
            ugen: None,
            box_qualify: Some(qualify),
        });
        s
    }

    #[test]
    fn merges_disjoint_egress_columns_with_per_column_provenance() {
        let a = shard("gw", "openai", "boxA", serde_json::json!({"box":"A"}), "2026-08-21T00-00-02Z");
        let b = shard("gw", "anthropic", "boxB", serde_json::json!({"box":"B"}), "2026-08-21T00-00-01Z");
        let m = merge_snapshots(&[a, b]).expect("disjoint shards merge");

        let mut keys: Vec<_> = m.matrix.upstreams.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, vec!["anthropic".to_string(), "openai".to_string()]);
        assert!(m.matrix.served, "served is OR across shards");
        // measured_at is the EARLIEST — it names the historical file.
        assert_eq!(m.measured_at, "2026-08-21T00-00-01Z");
        // Each column keeps the box that measured it, as fairness evidence.
        assert_eq!(m.matrix.upstreams["openai"].hardware.as_deref(), Some("boxA"));
        assert_eq!(m.matrix.upstreams["anthropic"].hardware.as_deref(), Some("boxB"));
        assert_eq!(
            m.matrix.upstreams["openai"].box_qualify,
            Some(serde_json::json!({"box":"A"}))
        );
        // Both columns' served cells survive the union.
        assert_eq!(served_cell_keys(&m.matrix).len(), 2);
    }

    #[test]
    fn rejects_overlapping_egress_columns() {
        let a = shard("gw", "openai", "boxA", serde_json::json!({}), "2026-08-21T00-00-01Z");
        let b = shard("gw", "openai", "boxB", serde_json::json!({}), "2026-08-21T00-00-02Z");
        assert_eq!(
            merge_snapshots(&[a, b]),
            Err(MergeError::OverlappingEgress {
                egress: "openai".to_string()
            })
        );
    }

    #[test]
    fn refuses_shards_that_are_not_the_same_experiment() {
        let base = shard("gw", "openai", "boxA", serde_json::json!({}), "2026-08-21T00-00-01Z");
        // Different gateway.
        let other = shard("OTHER", "anthropic", "boxB", serde_json::json!({}), "2026-08-21T00-00-02Z");
        assert_eq!(
            merge_snapshots(&[base.clone(), other]),
            Err(MergeError::Mismatch { field: "gateway" })
        );
        // Same gateway, different build.
        let mut b2 = shard("gw", "anthropic", "boxB", serde_json::json!({}), "2026-08-21T00-00-02Z");
        b2.build = "img:2".to_string();
        assert_eq!(
            merge_snapshots(&[base.clone(), b2]),
            Err(MergeError::Mismatch { field: "build" })
        );
        // Same gateway/build, different rig identity (release moved under the same tag).
        let mut b3 = shard("gw", "anthropic", "boxB", serde_json::json!({}), "2026-08-21T00-00-02Z");
        b3.rig.as_mut().unwrap().release_url = Some("moved".to_string());
        assert_eq!(
            merge_snapshots(&[base, b3]),
            Err(MergeError::Mismatch { field: "rig" })
        );
    }

    #[test]
    fn empty_shard_set_is_an_error() {
        assert_eq!(merge_snapshots(&[]), Err(MergeError::Empty));
    }

    #[test]
    fn single_shard_merges_to_itself() {
        let a = shard("gw", "openai", "boxA", serde_json::json!({"box":"A"}), "2026-08-21T00-00-01Z");
        let m = merge_snapshots(std::slice::from_ref(&a)).expect("one shard merges");
        assert_eq!(
            m.matrix.upstreams.keys().cloned().collect::<Vec<_>>(),
            vec!["openai".to_string()]
        );
        assert_eq!(m.matrix.upstreams["openai"].hardware.as_deref(), Some("boxA"));
    }

    #[test]
    fn shards_survive_a_json_round_trip_and_merge_from_disk() {
        // Mirrors the `otb merge` subcommand end to end, and exercises the new per-Upstream serde
        // fields (box_qualify/hardware) through a real write→read→merge→write.
        let dir = unique_dir("merge-from-disk");
        fs::create_dir_all(&dir).unwrap();
        let a = shard("gw", "openai", "boxA", serde_json::json!({"box":"A"}), "2026-08-21T00-00-02Z");
        let b = shard("gw", "anthropic", "boxB", serde_json::json!({"box":"B"}), "2026-08-21T00-00-01Z");
        for (i, s) in [&a, &b].iter().enumerate() {
            fs::write(
                dir.join(format!("shard{i}.json")),
                serde_json::to_string(s).unwrap(),
            )
            .unwrap();
        }
        let loaded: Vec<ResultSnapshot> = (0..2)
            .map(|i| {
                let text = fs::read_to_string(dir.join(format!("shard{i}.json"))).unwrap();
                serde_json::from_str(&text).unwrap()
            })
            .collect();
        let merged = merge_snapshots(&loaded).expect("shards merge from disk");

        let out = unique_dir("merge-from-disk-out");
        fs::create_dir_all(&out).unwrap();
        write_snapshot(&out, &merged).expect("merged snapshot writes");
        let written: ResultSnapshot =
            serde_json::from_str(&fs::read_to_string(out.join("gw.json")).unwrap()).unwrap();

        let mut keys: Vec<_> = written.matrix.upstreams.keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, vec!["anthropic".to_string(), "openai".to_string()]);
        assert_eq!(
            written.matrix.upstreams["openai"].hardware.as_deref(),
            Some("boxA")
        );
        assert_eq!(
            written.matrix.upstreams["anthropic"].box_qualify,
            Some(serde_json::json!({"box":"B"}))
        );
    }

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
                box_qualify: None,
                hardware: None,
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
                box_qualify: None,
                hardware: None,
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
                box_qualify: None,
                hardware: None,
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
