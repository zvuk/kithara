# Reuse & dependency audit (workspace-wide)

## Scope and method

I scanned all Rust files in the workspace and then manually inspected the largest duplication hotspots.

Commands used:

- `rg --files`
- `python` duplicate-window scan for `*.rs`
- `diff -u tests/src/hls_fixture/server.rs tests/tests/kithara_hls/fixture/server.rs`
- `diff -u tests/src/net_fixture.rs tests/tests/kithara_net/fixture.rs`
- `rg "tower::|tower_http|RetryLayer|ServiceBuilder" crates tests -g '*.rs'`

## High-impact reuse opportunities

### 1) `tests/src/net_fixture.rs` and `tests/tests/kithara_net/fixture.rs` are near-copies

The files are functionally identical (differences are mostly `pub` vs `pub(crate)`).

What can be improved:

- Keep one fixture module as source-of-truth (for example `tests/src/net_fixture.rs`).
- In per-suite tests, import from shared fixture instead of maintaining a parallel copy.
- If visibility causes friction, expose a minimal API from shared fixture and keep tests consuming only that API.

Expected benefit:

- One place to update helpers (`DelayedNet`, `success_stream`, `assert_success_all_net_methods`).
- Lower risk of tests diverging silently.

### 2) HLS test fixture duplication (`tests/src/hls_fixture/*` vs `tests/tests/kithara_hls/fixture/*`)

`server.rs` contains mostly the same logic in both locations; the diff is primarily visibility modifiers.

What can be improved:

- Consolidate into one shared fixture tree (`tests/src/hls_fixture/*`) and re-export where needed.
- Keep per-suite wrappers only when a suite truly needs suite-specific behavior.

Expected benefit:

- Less copy-paste in fixture server lifecycle and playlist generators.
- Easier synchronized changes for playlist/data generation.

### 3) Repeated retry loop body in `crates/kithara-net/src/retry.rs`

`get_bytes`, `stream`, `get_range`, and `head` each repeat the same retry-control flow.

What can be improved:

- Extract a private generic helper (for example `async fn run_with_retry<T, F, Fut>(...)`).
- Keep endpoint methods thin wrappers that pass operation closures.

Expected benefit:

- One retry semantics implementation to test and maintain.
- Lower risk of behavior drift between request methods.

### 4) Repeated read-for-concurrency loop in multi-instance tests

Very similar loops appear in:

- `tests/tests/multi_instance/concurrent_file.rs`
- `tests/tests/multi_instance/concurrent_hls.rs`
- `tests/tests/multi_instance/concurrent_mixed.rs`

What can be improved:

- Move common read loop into a shared test helper (for example under `tests/src/audio_fixture.rs`).
- Keep only scenario-specific setup/assertions in each test file.

Expected benefit:

- Better readability of scenario intent.
- Fewer places to adjust thresholds/timeouts.

## Library replacement opportunities

### A) Retry stack can likely reuse existing `tower` dependency

Workspace already has `tower`/`tower-http`, but retry logic is implemented manually in `kithara-net`.

Opportunity:

- Evaluate moving retry orchestration to `tower::retry` + custom `Policy`.

Trade-off:

- Might require adapting `Net` abstraction into `Service` boundaries.
- If adaptation is expensive, keep current approach but still deduplicate via private helper.

### B) Favor `kithara-test-utils` for shared HTTP/fixture server concerns

Workspace already includes `kithara-test-utils` (`http_server.rs`, `asset_server.rs`, `fixture_client.rs`, `fixture_protocol.rs`).

Opportunity:

- Pull common fixture server/session behavior out of duplicated local test fixtures into `kithara-test-utils`.

Trade-off:

- Requires careful API boundary so test-utils stays generic and not HLS-only.

## Suggested implementation order (smallest risk first)

1. Consolidate `net_fixture` duplicates.
2. Extract common multi-instance read helper.
3. Consolidate HLS fixture tree.
4. Deduplicate retry implementation internals.
5. Optional: assess migration to `tower::retry` if abstraction fit is acceptable.

