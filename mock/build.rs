// SPDX-License-Identifier: Apache-2.0
// Both measuring binaries use exactly the same provenance implementation.
#[path = "../engine/build.rs"]
mod stamp;
fn main() {
    stamp::main();
}
