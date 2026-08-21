// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The onthebench measurement engine.
//
// Ported from shell, for one reason above the others: the board's central rule is that an absent
// measurement publishes null with a reason and is never substituted by a number, and shell has no
// way to say "absent". Here it is a type, and the compiler will not let a caller forget it.
// unwrap/expect/panic are denied crate-wide so that a default value has to be written down where a
// reviewer can see it. Tests are the exception on purpose: a failing assertion IS a panic, so
// denying it there would only push tests into contortions that obscure what they check.
#![cfg_attr(test, allow(clippy::panic, clippy::unwrap_used, clippy::expect_used))]

pub mod cell;
pub mod config_lint;
pub mod frontier;
pub mod gen;
pub mod http;
pub mod ingress;
pub mod launch;
pub mod loadgen;
pub mod manifest;
pub mod measurement;
pub mod metric;
pub mod probe;
pub mod procsample;
pub mod qualify;
pub mod record;
pub mod reverify;
pub mod rigbound;
pub mod rss;
pub mod run;
pub mod search;
pub mod snapshot;
pub mod stats;
pub mod suite;
pub mod supervise;

pub use measurement::{Absent, Measurement};

/// A single process-wide lock for tests that mutate process-global env vars (MOCK_STREAM_INTERVAL_MS,
/// OTB_QUALIFY_BASELINE, ...). `cargo test` runs the crate's unit tests on many threads by default, so
/// a test that flips an env var another test reads is a data race with no code defect - a CI flake.
/// Every test that sets/removes such a var (and every test that reads one whose value it depends on)
/// takes this guard first, serialising them against each other while leaving env-free tests fully
/// parallel. Poison-tolerant: a test that panics while holding it must not wedge the rest of the suite.
#[cfg(test)]
pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
