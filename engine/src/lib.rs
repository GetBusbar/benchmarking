// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The onthebench measurement engine.
//
// An absent measurement must publish null with a reason, never a substituted number; `Absent` makes
// that a type instead of a convention. unwrap/expect/panic are denied crate-wide so a default value
// has to be written down where a reviewer can see it; tests are exempted since a failing assertion is
// meant to panic.
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
