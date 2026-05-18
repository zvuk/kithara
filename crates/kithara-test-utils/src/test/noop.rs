// No-op stub when `feature = "test"` is disabled. The contents of
// `test::real` are reachable only from `#[cfg(test)]` blocks or from the
// integration-tests crate — both of which enable the `test` feature. No
// production code path imports from `kithara_test_utils::test::*`, so
// leaving this module empty keeps the lib compilable without the heavy
// fixture deps (axum, reqwest, tempfile, etc.).
