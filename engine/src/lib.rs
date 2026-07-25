// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The onthebench measurement engine.
//
// Ported from shell, for one reason above the others: the board's central rule is that an absent
// measurement publishes null with a reason and is never substituted by a number, and shell has no
// way to say "absent". Here it is a type, and the compiler will not let a caller forget it.
pub mod measurement;

pub use measurement::{Absent, Measurement};
