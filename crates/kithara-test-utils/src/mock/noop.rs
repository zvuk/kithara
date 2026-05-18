// No-op stub when `feature = "mock"` is disabled. `unimock` has a
// large, macro-generated API surface (Unimock, MockFn, matching!,
// some_call, Clause, …) whose trait bounds and lifetimes do not admit
// a faithful no-op replacement. No production code path imports from
// `kithara_test_utils::mock::*`; consumers needing the runtime depend
// on `unimock` directly. Module is intentionally empty.
