// The mock carries the same instrument stamp as the engine, so the rig can ask the binary which
// commit it was built from instead of trusting a label beside it.
#[path = "../engine/build.rs"]
mod stamp;
fn main() {
    stamp::main();
}
