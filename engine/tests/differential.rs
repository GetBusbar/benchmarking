// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// DIFFERENTIAL PARITY: run the shell implementation and the Rust one over the SAME generated inputs
// and require identical answers.
//
// A side-by-side reading of these functions is not enough. Two humans-plus-agents read the two
// search implementations line by line and still missed four defects, including two that
// reintroduced bugs the shell had already found and fixed. A diff over random inputs finds that
// class immediately, because it does not depend on anyone noticing which line matters.
//
// This is migration scaffolding on purpose. It requires bash and the shell library to exist, so it
// skips cleanly once they are deleted, which is the signal that the port is complete.

// An integration test is its own crate, so the crate-level test allow in src/lib.rs does not reach
// it. The same reasoning applies here: a failing assertion IS a panic, and denying that only pushes
// a test into contortions that hide what it checks.
#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(Path::to_path_buf).unwrap_or_default()
}

fn shell_available() -> bool {
    repo_root().join("lib/plateau.sh").exists()
}

fn otb_bin() -> PathBuf {
    // cargo puts integration-test binaries in target/<profile>/deps, so the binary is two up.
    let mut p = std::env::current_exe().unwrap_or_default();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("otb")
}

/// Deterministic pseudo-random series, so a failure reproduces exactly from its seed.
fn lcg(state: &mut u64) -> f64 {
    *state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    ((*state >> 11) as f64) / ((1u64 << 53) as f64)
}

/// A memory series with a controllable slope and noise: the shapes the plateau gate actually judges.
fn series(seed: u64, n: usize, base: f64, slope_per_min: f64, noise: f64) -> String {
    let mut st = seed.wrapping_mul(2_862_933_555_777_941_757).wrapping_add(3_037_000_493);
    let mut out = String::new();
    for i in 0..n {
        let t = i as f64 * 3.0;
        let v = base + slope_per_min * (t / 60.0) + (lcg(&mut st) - 0.5) * 2.0 * noise;
        out.push_str(&format!("{t} {v:.4}\n"));
    }
    out
}

fn run_shell(func: &str, input: &str, args: &[&str]) -> String {
    let root = repo_root();
    let path = tempfile(input);
    // Both paths are QUOTED. They were not, and the temp filename briefly contained spaces, so bash
    // word-split it, handed plateau_check a path that did not exist, and it dutifully returned 0 for
    // every single case. The harness reported total disagreement and the port was fine. Quote first,
    // then trust a differential result.
    let script = format!(
        "set -u; . '{}/lib/plateau.sh'; {} '{}' {}",
        root.display(),
        func,
        path.display(),
        args.join(" ")
    );
    let out = Command::new("bash").arg("-c").arg(&script).output().expect("bash must run");
    let _ = std::fs::remove_file(&path);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A temp file whose name contains nothing that a shell would split on.
fn tempfile(contents: &str) -> PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    let mut path = std::env::temp_dir();
    path.push(format!("otb-diff-{pid}-{n}.series"));
    let mut f = std::fs::File::create(&path).expect("temp file");
    f.write_all(contents.as_bytes()).expect("write temp");
    f.flush().expect("flush temp");
    path
}

fn run_rust(sub: &str, input: &str, args: &[&str]) -> String {
    let mut child = Command::new(otb_bin())
        .arg(sub)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("otb binary must exist: run `cargo build --bin otb` first");
    child.stdin.as_mut().expect("stdin").write_all(input.as_bytes()).expect("write stdin");
    let out = child.wait_with_output().expect("otb must run");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn plateau_check_agrees_with_the_shell_over_many_shapes() {
    if !shell_available() {
        eprintln!("shell engine gone: differential parity no longer applicable");
        return;
    }
    let mut disagreements = Vec::new();
    let mut compared = 0;
    for seed in 0..60u64 {
        // Sweep the shapes the gate exists to separate: flat, slow leak, fast leak, falling, noisy.
        for &(slope, noise) in &[
            (0.0, 0.0),
            (0.0, 0.4),
            (0.5, 0.1),
            (2.4, 0.1),   // near the documented 60s-window boundary
            (5.0, 0.1),   // clearly leaking
            (-3.0, 0.1),  // falling: the shell's drift test is ONE-SIDED, so this must still pass
            (0.0, 3.0),   // range-gate territory
        ] {
            for &n in &[4usize, 7, 21] {
                // THRESHOLDS ARE SWEPT, not left at the defaults. For a linear series
                // spread% = 2 x drift%, so at the shipped (trend=1, range=2) the two gates bind
                // identically and the range test MASKS the drift test completely. A perturbation
                // that made the drift test two-sided passed this harness unnoticed until the
                // thresholds were separated. A wide range isolates drift; a wide trend isolates
                // spread.
                for &(trend, range) in &[("1", "2"), ("1", "99"), ("99", "2"), ("0.5", "50")] {
                    let input = series(seed, n, 120.0, slope, noise);
                    let sh = run_shell("plateau_check", &input, &[trend, range]);
                    let rs = run_rust("plateau-check", &input, &[trend, range]);
                    compared += 1;
                    if sh != rs {
                        disagreements.push(format!(
                            "seed={seed} n={n} slope={slope} noise={noise} trend={trend} range={range}: shell={sh:?} rust={rs:?}"
                        ));
                    }
                }
            }
        }
    }
    assert!(compared > 0);
    assert!(
        disagreements.is_empty(),
        "{} of {} cases disagree:\n{}",
        disagreements.len(),
        compared,
        disagreements.join("\n")
    );
}

#[test]
fn growth_rate_agrees_with_the_shell_to_the_shell_s_own_precision() {
    if !shell_available() {
        return;
    }
    let mut disagreements = Vec::new();
    for seed in 0..40u64 {
        for &slope in &[0.0, 0.25, 2.4, 10.0, -5.0, 64.9] {
            for &n in &[2usize, 5, 21] {
                let input = series(seed, n, 120.0, slope, 0.05);
                let sh = run_shell("plateau_growth_rate", &input, &[]);
                let rs = run_rust("growth-rate", &input, &[]);
                // Both print %.3f, so this is a string compare on purpose: it catches a formatting
                // divergence as well as a numeric one, and archived output is compared as text.
                if sh != rs {
                    disagreements.push(format!("seed={seed} n={n} slope={slope}: shell={sh:?} rust={rs:?}"));
                }
            }
        }
    }
    assert!(disagreements.is_empty(), "growth rate disagrees:\n{}", disagreements.join("\n"));
}

#[test]
fn window_agrees_with_the_shell() {
    if !shell_available() {
        return;
    }
    let mut disagreements = Vec::new();
    for seed in 0..20u64 {
        for &w in &["30", "60", "9999"] {
            let input = series(seed, 40, 120.0, 1.0, 0.2);
            let sh = run_shell("plateau_window", &input, &[w]);
            let rs = run_rust("window", &input, &[w]);
            // Compare the timestamps kept, not the float text: the shell echoes its input lines
            // verbatim while the Rust reprints parsed floats.
            let keys = |s: &str| {
                s.lines()
                    .filter_map(|l| l.split_whitespace().next().map(str::to_string))
                    .collect::<Vec<_>>()
            };
            if keys(&sh) != keys(&rs) {
                disagreements.push(format!("seed={seed} w={w}: shell kept {:?}, rust kept {:?}", keys(&sh), keys(&rs)));
            }
        }
    }
    assert!(disagreements.is_empty(), "window disagrees:\n{}", disagreements.join("\n"));
}
