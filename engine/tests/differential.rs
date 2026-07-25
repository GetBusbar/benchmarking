// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// DIFFERENTIAL PARITY: run the shell implementation and the Rust implementation over the same
// generated inputs and require the same answer.
//
// A side-by-side reading of these modules missed six defects, four of them in the searches, two of
// them reintroductions of bugs the shell had already found and fixed. Reading finds what a reader
// thinks to look for. A diff over random inputs finds what nobody thought of, which is the whole
// remaining risk once the code has been reviewed once.
//
// Skipped rather than failed when bash is unavailable: this must never be the reason a build breaks
// on a machine that simply has no shell to compare against.

use std::io::Write;
use std::process::{Command, Stdio};

fn repo_root() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR is <root>/engine
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().map(|p| p.to_path_buf()).unwrap_or_default()
}

fn otb_bin() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // <root>/target/debug/deps/differential-<hash> -> <root>/target/debug/otb
    let dir = exe.parent()?.parent()?;
    let bin = dir.join("otb");
    bin.exists().then_some(bin)
}

/// Run a shell function from lib/plateau.sh over a samples file.
///
/// The series file name is UNIQUE PER CALL. The first version of this harness derived it from the
/// process id alone, so the three test functions (which cargo runs on parallel threads) all wrote
/// the same path and read each other's data: every comparison was then between one implementation's
/// answer and the other's answer to a DIFFERENT series. It failed loudly and looked like a swarm of
/// parity defects. A differential harness that races is worse than none, because its output is
/// indistinguishable from the bugs it exists to find.
fn shell(func_call: &str, samples: &str) -> Option<String> {
    let root = repo_root();
    let series = unique_series_path()?;
    std::fs::write(&series, samples).ok()?;
    let script = format!(
        "set -u; . '{}/lib/plateau.sh'; {}",
        root.display(),
        func_call.replace("{F}", &series.display().to_string())
    );
    let out = Command::new("bash").arg("-c").arg(&script).output().ok()?;
    let _ = std::fs::remove_file(&series);
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

static SERIES_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn unique_series_path() -> Option<std::path::PathBuf> {
    let mut base = std::env::temp_dir();
    base.push(format!("otb-diff-{}", std::process::id()));
    std::fs::create_dir_all(&base).ok()?;
    let n = SERIES_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Some(base.join(format!("series-{n}.txt")))
}

fn rust(args: &[&str], samples: &str) -> Option<String> {
    let bin = otb_bin()?;
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    child.stdin.as_mut()?.write_all(samples.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn have_prereqs() -> bool {
    Command::new("bash").arg("-c").arg("true").output().is_ok() && otb_bin().is_some()
}

/// A deterministic generator, so any failure reproduces exactly from its seed.
struct Lcg(u64);
impl Lcg {
    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }
    fn f64_in(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (self.next_u32() as f64 / u32::MAX as f64) * (hi - lo)
    }
}

/// Series shapes that matter: flat, rising, falling, asymptoting, noisy, spiky, degenerate.
fn generate(seed: u64, case: u32) -> String {
    let mut r = Lcg(seed);
    let n = 4 + (r.next_u32() % 25) as usize;
    let base = r.f64_in(20.0, 400.0);
    let mut out = String::new();
    // One cadence for the whole series, drawn once: real samples arrive on a fixed interval, and a
    // non-monotonic time axis is not a case either implementation is specified to handle.
    let dt = r.f64_in(1.0, 6.0).round().max(1.0);
    for i in 0..n {
        let t = i as f64 * dt;
        let v = match case % 6 {
            0 => base + r.f64_in(-0.3, 0.3),                          // flat with noise
            1 => base + i as f64 * r.f64_in(0.05, 4.0),               // rising
            2 => base - i as f64 * r.f64_in(0.05, 2.0),               // falling
            3 => base + (1.0 - (-(i as f64) / 4.0).exp()) * 40.0,     // asymptoting leak
            4 => base + if i % 2 == 0 { 1.5 } else { -1.5 },          // oscillating
            _ => base + if i == n / 2 { 50.0 } else { 0.0 },          // single spike
        };
        out.push_str(&format!("{t} {v:.4}\n"));
    }
    out
}

#[test]
fn plateau_check_agrees_with_the_shell_over_generated_series() {
    if !have_prereqs() {
        eprintln!("skipping: bash or the otb binary is unavailable");
        return;
    }
    let mut mismatches = Vec::new();
    for seed in 0..240u64 {
        let samples = generate(seed, seed as u32);
        let sh = shell("plateau_check '{F}' 1 2", &samples);
        let rs = rust(&["plateau-check", "1", "2"], &samples);
        if let (Some(sh), Some(rs)) = (sh, rs) {
            if sh != rs {
                mismatches.push(format!("seed {seed}: shell={sh} rust={rs}\n{samples}"));
            }
        }
    }
    assert!(mismatches.is_empty(), "plateau verdicts diverged:\n{}", mismatches.join("\n---\n"));
}

#[test]
fn growth_rate_agrees_with_the_shell_over_generated_series() {
    if !have_prereqs() {
        eprintln!("skipping: bash or the otb binary is unavailable");
        return;
    }
    let mut mismatches = Vec::new();
    for seed in 500..680u64 {
        let samples = generate(seed, seed as u32);
        let sh = shell("plateau_growth_rate '{F}'", &samples);
        let rs = rust(&["growth-rate"], &samples);
        if let (Some(sh), Some(rs)) = (sh, rs) {
            // Both print to 3dp. Compare numerically to tolerate a last-digit rounding difference
            // between awk's printf and Rust's, which is formatting, not disagreement.
            match (sh.parse::<f64>(), rs.parse::<f64>()) {
                (Ok(a), Ok(b)) if (a - b).abs() <= 0.002 => {}
                (Err(_), Err(_)) => {} // both unmeasurable, both printed nothing
                _ => mismatches.push(format!("seed {seed}: shell='{sh}' rust='{rs}'\n{samples}")),
            }
        }
    }
    assert!(mismatches.is_empty(), "growth rates diverged:\n{}", mismatches.join("\n---\n"));
}

#[test]
fn window_agrees_with_the_shell_over_generated_series() {
    if !have_prereqs() {
        eprintln!("skipping: bash or the otb binary is unavailable");
        return;
    }
    let mut mismatches = Vec::new();
    for seed in 900..1020u64 {
        let samples = generate(seed, seed as u32);
        for w in [10.0f64, 30.0, 60.0, 1e9] {
            let sh = shell(&format!("plateau_window '{{F}}' {w}"), &samples);
            let rs = rust(&["window", &w.to_string()], &samples);
            if let (Some(sh), Some(rs)) = (sh, rs) {
                // Compare the kept TIMESTAMPS: the shell echoes its input lines verbatim while the
                // Rust reprints parsed floats, so the text differs even when the selection agrees.
                let keys = |s: &str| -> Vec<String> {
                    s.lines()
                        .filter_map(|l| l.split_whitespace().next())
                        .filter_map(|t| t.parse::<f64>().ok())
                        .map(|t| format!("{t:.3}"))
                        .collect()
                };
                if keys(&sh) != keys(&rs) {
                    mismatches.push(format!("seed {seed} w={w}: shell={:?} rust={:?}", keys(&sh), keys(&rs)));
                }
            }
        }
    }
    assert!(mismatches.is_empty(), "windows diverged:\n{}", mismatches.join("\n"));
}
