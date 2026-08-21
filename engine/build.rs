//! Bake the commit this engine was built from INTO the binary.
//!
//! WHY THIS EXISTS. The bench boxes do not build the engine; they download a prebuilt `otb` from the
//! rolling `rig` release (lib/rig.sh, run-on-ec2.sh). The commit recorded in every artifact came from
//! `BENCH_ENGINE_COMMIT`, an environment variable the orchestrator set from its own checkout - so the
//! stamp said what the operator INTENDED to run, and nothing anywhere compared it to the binary that
//! actually ran.
//!
//! On 2026-08-03 that produced a snapshot stamped `0ce7a907` measured by an engine that did not
//! contain a single line of `0ce7a907`: the fixes had been pushed to a branch, the release only
//! rebuilds on a push to `main`, and the boxes fetched the previous binary. Proven by grepping the
//! binary on the box for two strings that exist only in the newer commit; both absent.
//!
//! A stamp that cannot be checked against the artifact can claim any engine at all. So the binary now
//! carries its own provenance, `otb engine-commit` reads it back, and the run refuses to measure when
//! that disagrees with the commit it is about to stamp.
//!
//! `OTB_BUILD_COMMIT` wins when set, because the release build is the case that matters and CI knows
//! its own sha exactly. Falling back to `git rev-parse HEAD` keeps a local build honest, and an empty
//! string is the honest answer when neither is available - never a guess, and never a value that
//! could be mistaken for a real commit.

fn main() {
    let commit = std::env::var("OTB_BUILD_COMMIT")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let commit = commit.unwrap_or_else(|| {
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    });
    println!("cargo:rustc-env=OTB_ENGINE_COMMIT={commit}");
    // Re-bake when HEAD moves, so a rebuild after a commit does not keep the old sha. Both paths are
    // named because a detached HEAD (a worktree, a CI checkout) writes the sha into .git/HEAD itself.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");
    println!("cargo:rerun-if-env-changed=OTB_BUILD_COMMIT");
}
