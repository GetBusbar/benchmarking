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

pub mod measurement;
pub mod probe;
pub mod qualify;
pub mod record;
pub mod rss;
pub mod search;
pub mod stats;

pub use measurement::{Absent, Measurement};
