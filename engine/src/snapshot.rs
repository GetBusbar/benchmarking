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
    /// Qualification rejected the machine before any gateway grid was measured.
    BoxQualificationFailed {
        verdict: serde_json::Value,
    },
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Json(serde_json::Error),
    /// The incoming snapshot does not serve every cell the one already on disk at the current path
    /// serves (for two grid-shaped snapshots: some previously served cell is no longer served, even if
    /// others were gained; across a v1/v2 shape change, which has no shared cell identity: it serves
    /// fewer cells). Refusing here is the whole point: a boot failure that served nothing must never
    /// overwrite a prior run that served most of the grid just because it happened to run later.
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
            SnapshotError::BoxQualificationFailed { verdict } => write!(f, "BOX_QUALIFICATION_FAILED: {verdict}; refusing to measure the gateway grid"),
            SnapshotError::Io { path, source } => {
                write!(f, "snapshot io error at {}: {source}", path.display())
            }
            SnapshotError::Json(source) => write!(f, "snapshot json error: {source}"),
            SnapshotError::PromoteGuard { existing_served, incoming_served } => write!(
                f,
                "refusing to overwrite a snapshot that served {existing_served} cells: the incoming one \
                 (serving {incoming_served}) no longer serves every cell the existing one did"
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
            SnapshotError::BoxQualificationFailed { .. }
            | SnapshotError::PromoteGuard { .. }
            | SnapshotError::UnsafeName { .. }
            | SnapshotError::UnresolvableHeader { .. } => None,
        }
    }
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

    if let Err(source) = fs::rename(&tmp_path, target) {
        // The same cleanup as a failed write: a rename that did not happen leaves a full copy of the
        // snapshot at a hidden path in the results directory, which a later sweep could pick up.
        let _ = fs::remove_file(&tmp_path);
        return Err(SnapshotError::Io {
            path: target.to_path_buf(),
            source,
        });
    }

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
/// If a current file already exists and `snapshot` does not serve every cell it served (see
/// `SnapshotError::PromoteGuard` for the exact rule), this returns that error and writes NEITHER file: a rewrite of the promote guard's own
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

#[cfg(test)]
#[path = "tests/snapshot_tests.rs"]
mod tests;
