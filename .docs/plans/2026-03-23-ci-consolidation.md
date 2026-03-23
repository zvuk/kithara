# CI Consolidation & Multi-Platform Release

## Current State

14 → 6 jobs already consolidated. Need to further merge and add release jobs.

## Target Pipeline

```
build → lint → test → release-*
```

### Stage: build (~30 min cold, ~2 min warm)
One container, compiles everything, saves cache.

- `just test --no-run` (nextest, test-release profile)
- `RUSTUP_TOOLCHAIN=1.89 cargo check --workspace` (MSRV)
- `just wasm-check` (wasm32 target)
- `just wasm-size-check` (nightly + wasm-slim)

### Stage: lint (~5 min with cache)
One container, all checks on pre-compiled code.

- `just lint-full` (fmt, clippy, ast-grep, xtask tests, quality audits)
- `just quality-report-ci`
- `just deny` + `just machete` + `just arch`
- `just hack` (moved from test stage — it's a check, not a test)
- `just semver` (conditional, on crate source changes)

### Stage: test (~15 min with cache)
One container, runs tests + coverage + perf + bench.

- `just coverage` (nextest under llvm-cov, test-release profile)
- `just perf-test` (release, ignored perf tests)
- `just bench-ci` (criterion, conditional on ABR/bench changes)

### Stage: release (parallel, each on own runner)
Multiple containers, one per platform. Runs after test passes.
**Trigger: always** (not just tags) — artifacts downloadable from any pipeline.

#### release-apple (runner: macos)
- `just xcframework` → XCFramework (iOS + macOS, arm64 + x86_64)
- zip + checksum for SPM
- Upload to GitLab package registry
- Create GitLab release (on tag only)
- Artifacts: `KitharaFFIInternal.xcframework.zip`

**SPM distribution**: `apple/Package.swift` already supports:
- `KITHARA_LOCAL_DEV=1` — use local xcframework
- `KITHARA_BINARY_BASE_URL` — override download host
- Default: GitHub releases URL

For tag releases, CI updates `version` and `checksum` in `Package.swift`,
pushes the commit, uploads binary to package registry, creates release.

#### release-android (runner: cce-feature + NDK)
- `just android-aar` → AAR with JNI libs (arm64-v8a, armeabi-v7a, x86_64)
- Artifacts: `android/lib/build/outputs/aar/lib-release.aar`

**Requirement**: Android NDK must be available on runner.
Option A: Custom Docker image with NDK pre-installed.
Option B: Install NDK in before_script.

#### release-wasm (runner: cce-feature)
- `cargo xtask wasm build --release` → dist/ with .wasm + .js + index.html
- Artifacts: `crates/kithara-wasm/dist/`

#### release-desktop (runner: cce-feature)
- `cargo build -p kithara-app --release --features tui`
- Artifacts: `target/release/kithara` (Linux binary)

**macOS binary**: built on macos runner alongside xcframework.
**Windows binary**: requires cross-compilation or Windows runner (defer).

## Job Count

| Before | After |
|--------|-------|
| 14 jobs, 14 containers | 4 stages, 7 jobs (build + lint + test + 4 release) |

## Key Decisions Needed

1. **Android NDK**: custom Docker image or install in CI?
2. **Windows**: cross-compile from Linux or defer?
3. **Release trigger**: always build artifacts, or only on tags/branches?
4. **SPM auto-update**: should CI auto-commit Package.swift version+checksum on tag?

## Implementation Order

1. Merge hack+semver into lint, bench into test (quick)
2. Add release-wasm job (no special runner needed)
3. Add release-desktop job (Linux binary, no special runner)
4. Fix release-apple job (enable always, not just tags)
5. Add release-android job (needs NDK investigation)
6. Windows binary (defer — needs cross or runner)
