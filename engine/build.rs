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

pub fn main() {
    let commit = std::env::var("OTB_BUILD_COMMIT")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let commit = commit.unwrap_or_else(|| {
        let head = git(&["rev-parse", "HEAD"]).unwrap_or_default();
        if head.is_empty() {
            return head;
        }
        // A LOCAL BUILD OF UNCOMMITTED EDITS IS NOT THAT COMMIT. HEAD's sha alone stamped a binary
        // built from a modified engine/ or mock/ as the clean commit, and the `otb engine-commit`
        // check this file exists for then passed for an engine that is not it. The suffix makes the
        // check fail instead; a tree whose state cannot be read is not claimed clean either.
        match git(&[
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--",
            "engine",
            "mock",
            "Cargo.toml",
            "Cargo.lock",
            ".cargo",
            "rust-toolchain.toml",
        ]) {
            Some(s) if s.is_empty() => head,
            Some(_) => format!("{head}-dirty"),
            None => format!("{head}-unverified"),
        }
    });
    println!("cargo:rustc-env=OTB_ENGINE_COMMIT={commit}");
    println!(
        "cargo:rustc-env=OTB_SOURCE_COMMIT={}",
        git(&["rev-parse", "HEAD"]).unwrap_or_default()
    );
    // Re-bake when HEAD moves, so a rebuild after a commit does not keep the old sha. Both paths are
    // named because a detached HEAD (a worktree, a CI checkout) writes the sha into .git/HEAD itself.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");
    // And when the tree's own state moves, so the dirty suffix appears on an edit and clears on the
    // commit (which rewrites the index).
    println!("cargo:rerun-if-changed=../.git/index");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=../engine/src");
    println!("cargo:rerun-if-changed=../mock/src");
    for path in [
        "../Cargo.toml",
        "../Cargo.lock",
        "../.cargo",
        "../rust-toolchain.toml",
        "../engine/build.rs",
        "../mock/build.rs",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=OTB_BUILD_COMMIT");
}

/// A git command's trimmed stdout, or `None` when git is absent or the command fails.
fn git(args: &[&str]) -> Option<String> {
    std::process::Command::new("git")
        .arg("-C")
        .arg(std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").ok()?).parent()?)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
}
