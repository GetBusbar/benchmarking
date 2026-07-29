// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE CLI CONTRACT of the `otb` binary, from outside the process.
//
// The shell-parity subcommands (plateau-check / growth-rate / window) are the seam a differential
// harness diffs against the shell originals, and the run/loadgen entry points are how a whole field
// run begins - so a wrong exit code or a wrong byte on stdout here is not cosmetic, it is a shell
// `if` branch in the orchestrator taking the wrong arm. Nothing outside end_to_end.rs drove the
// binary's argument handling at all.
//
// The single most load-bearing assertion in this file: `growth-rate` on unmeasurable input prints
// NOTHING. The absence discipline the whole engine is built on crosses a process boundary right
// here, where the type system cannot follow it - printing "0.000" instead would hand every shell
// consumer a measured zero for a rate that was never taken.

// The crate denies unwrap/expect/panic so a measurement defect can never abort a run. A test is the
// opposite case: failures must be loud. Scoped to this file, same as engine/tests/end_to_end.rs.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::io::Write;
use std::process::{Command, Output, Stdio};

fn otb() -> Command {
    Command::new(env!("CARGO_BIN_EXE_otb"))
}

fn run_with_stdin(args: &[&str], stdin: &str) -> Output {
    let mut child = otb()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn otb");
    child
        .stdin
        .as_mut()
        .expect("piped stdin")
        .write_all(stdin.as_bytes())
        .expect("write stdin");
    child.wait_with_output().expect("wait for otb")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// A flat series and a steeply rising one, in the "<t_s> <mib>" stdin shape the shell writes.
fn series(rate_per_sample: f64, n: usize) -> String {
    (0..n)
        .map(|i| format!("{} {}\n", i * 2, 100.0 + rate_per_sample * i as f64))
        .collect()
}

#[test]
fn no_arguments_is_usage_on_stderr_and_exit_code_2() {
    let out = otb().output().expect("run otb");
    assert_eq!(
        out.status.code(),
        Some(2),
        "the no-subcommand case is a usage error, exit 2 by unix convention"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    for cmd in ["plateau-check", "growth-rate", "window", "version"] {
        assert!(
            err.contains(cmd),
            "the usage text must name every stdin subcommand, missing {cmd}: {err}"
        );
    }
    assert!(
        out.stdout.is_empty(),
        "a usage error writes nothing to stdout, which a pipeline may be capturing"
    );
}

#[test]
fn an_unknown_subcommand_is_refused_not_silently_ignored() {
    let out = otb().arg("frobnicate").output().expect("run otb");
    assert_eq!(out.status.code(), Some(2), "unknown subcommand is exit 2");
}

#[test]
fn version_prints_the_crate_version_and_nothing_else() {
    let out = otb().arg("version").output().expect("run otb");
    assert!(out.status.success());
    assert_eq!(
        stdout_of(&out).trim(),
        env!("CARGO_PKG_VERSION"),
        "the version stamp is the one artifact identifier a release has"
    );
}

// ---- plateau-check: prints exactly "1" or "0", the shell's own contract -------------------------

#[test]
fn plateau_check_prints_1_for_a_flat_series_and_0_for_a_rising_one() {
    let flat = run_with_stdin(&["plateau-check", "1.0", "2.0"], &series(0.0, 30));
    assert!(flat.status.success());
    assert_eq!(
        stdout_of(&flat).trim(),
        "1",
        "a flat series certifies steady"
    );

    let rising = run_with_stdin(&["plateau-check", "1.0", "2.0"], &series(1.0, 30));
    assert!(rising.status.success());
    assert_eq!(
        stdout_of(&rising).trim(),
        "0",
        "a rising series must never certify steady"
    );
}

#[test]
fn plateau_check_on_too_few_samples_prints_0_never_1() {
    // Undecidable is not steady: a shell consumer branching on "1" must not treat a three-line
    // series as a settled gateway.
    for input in ["", "0 100\n2 100\n4 100\n"] {
        let out = run_with_stdin(&["plateau-check", "1.0", "2.0"], input);
        assert!(out.status.success());
        assert_eq!(
            stdout_of(&out).trim(),
            "0",
            "{} samples cannot certify a plateau",
            input.lines().count()
        );
    }
}

// ---- growth-rate: the absence discipline at the process boundary --------------------------------

#[test]
fn growth_rate_prints_the_fitted_slope_to_three_decimals() {
    // 0.5 MiB per 2 s sample = 15 MiB/min, exactly, and %.3f is the shell printf it must match.
    let out = run_with_stdin(&["growth-rate"], &series(0.5, 30));
    assert!(out.status.success());
    assert_eq!(stdout_of(&out).trim(), "15.000");
}

#[test]
fn an_unmeasurable_growth_rate_prints_nothing_never_a_zero() {
    // THE BOUNDARY WHERE Measurement<T> ENDS. Empty input and a single sample have no defined
    // slope; the shell's awk exits printing nothing, and this must match byte for byte - "0.000"
    // here would be a measured zero invented for a rate that was never taken, the exact defect the
    // whole engine exists to prevent, escaping through its own CLI.
    for input in ["", "0 100\n"] {
        let out = run_with_stdin(&["growth-rate"], input);
        assert!(out.status.success());
        assert_eq!(
            stdout_of(&out),
            "",
            "an absent rate must print NOTHING, not a number, for input {input:?}"
        );
    }
}

#[test]
fn a_flat_series_growth_rate_is_a_printed_zero_distinct_from_absence() {
    // The other half of the contract: a real measured zero DOES print. Collapsing it into the
    // silent case would make "no leak" indistinguishable from "no data".
    let out = run_with_stdin(&["growth-rate"], &series(0.0, 30));
    assert!(out.status.success());
    assert_eq!(
        stdout_of(&out).trim(),
        "0.000",
        "a measured zero rate prints, only an absent one is silent"
    );
}

// ---- window: trailing-seconds selection, and hostile stdin --------------------------------------

#[test]
fn window_keeps_only_the_trailing_seconds_anchored_to_the_last_sample() {
    // 60 samples at 2 s spacing = 118 s of history; a 60 s window keeps the last 31 (t >= 58).
    let out = run_with_stdin(&["window", "60"], &series(0.0, 60));
    assert!(out.status.success());
    let text = stdout_of(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        31,
        "the trailing 60 s of a 2 s series is 31 samples"
    );
    assert!(
        lines[0].starts_with("58 "),
        "the window is anchored to the last sample's own clock: {:?}",
        lines.first()
    );
}

#[test]
fn malformed_stdin_lines_are_skipped_never_parsed_into_samples() {
    // A garbage line must not become a sample: the series files this reads are written by shell
    // pipelines, and one stray log line turning into a (t, mib) point would silently bend the fit.
    let input = "0 100\nnot a sample\n2 100\n4\n6 100 extra-is-fine\n8 100\n";
    let out = run_with_stdin(&["window", "9999"], input);
    assert!(out.status.success());
    assert_eq!(
        stdout_of(&out).lines().count(),
        4,
        "exactly the four parseable lines survive"
    );
}

// ---- the load-bearing entry points refuse bad invocations before doing anything -----------------

#[test]
fn loadgen_with_an_unparsable_address_is_a_usage_error_not_a_window() {
    let out = otb()
        .args(["loadgen", "not-an-address"])
        .output()
        .expect("run otb");
    assert_eq!(
        out.status.code(),
        Some(2),
        "a loadgen that cannot know its target must not run a window"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("usage: otb loadgen"),
        "the refusal explains the expected shape"
    );
}

#[test]
fn run_without_a_manifest_is_a_usage_error_and_a_missing_manifest_is_a_failure() {
    let bare = otb().arg("run").output().expect("run otb");
    assert_eq!(
        bare.status.code(),
        Some(2),
        "no manifest path is a usage error"
    );

    let missing = otb()
        .args(["run", "/nonexistent/definition.json", "127.0.0.1:9", "/tmp"])
        .output()
        .expect("run otb");
    assert!(
        !missing.status.success(),
        "a manifest that cannot be loaded must fail, not measure a gateway it cannot describe"
    );
    assert!(
        !missing.stderr.is_empty(),
        "the failure says why, on stderr"
    );
}

#[test]
fn smoke_without_addresses_is_a_usage_error() {
    let out = otb().arg("smoke").output().expect("run otb");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: otb smoke"));
}
