/// Dump the flash quiescence-engine state to stderr. The `#[kithara::test]`
/// harness calls this from both hang exits (virtual-timeout panic and the
/// HARD TIMEOUT abort thread) so a wedged run self-reports every parked
/// participant, deadline and pending signal instead of dying opaque.
/// Thin forward into the platform control surface (a no-op without the
/// `flash` feature or on wasm).
pub fn flash_dump_to_stderr(context: &str) {
    kithara_platform::flash::dump_to_stderr(context);
}
