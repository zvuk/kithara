# Kithara — coding rules for autonomous agents

## 0.f) No speculative code / no future-proofing
**Rule (normative):** agents **must not** add code "for the future" that is unused within the current task/checkbox and not part of a public contract.

Prohibited:
- Adding unused helper functions/methods/fields "just in case";
- Adding "introspection", "debug helpers", "potential extensions" unless used right now;
- Leaving "convenience" methods without tests or usage;
- Adding alternative branches/behavior variants without an explicit task requirement.

## 0.d) Dependency hygiene: no duplicates
**Rule:** agents **must not** add a new dependency if the needed functionality already exists in the workspace or the standard library.

Before adding any new crate:
1) Check `kithara/Cargo.toml` (`[workspace.dependencies]`) — a suitable library may already be present;
2) Prefer what already exists (e.g., use `tracing` for logging, do not add `log`);
3) If an equivalent is already available — **do not add** a new dependency.

If a new dependency is unavoidable:
- Justify the need in the task/PR description (1-2 sentences explaining what current dependencies do not cover);
- Add it **first** to `[workspace.dependencies]`, then reference it in the target crate as `{ workspace = true }`.

## 0) Core principles

- Minimal magic and hidden dependencies.
- Predictability, testability, reproducibility.
- Components must be loosely coupled and replaceable.
- Code is the source of truth. Longer explanations go in the `README` of the corresponding subcrate.

## 1) Dependency management (workspace-first)

**Rule:** all dependencies are added to the workspace first, then referenced in individual crates.

### 1.1 Where to declare versions
- Dependency versions are declared in the root `Cargo.toml` workspace under:
  - `[workspace.dependencies]`
- This applies to all dependency kinds:
  - regular (`dependencies`)
  - dev (`dev-dependencies`)
  - build (`build-dependencies`)

### 1.2 How to reference in crates
- In a crate's `Cargo.toml`, reference dependencies without a version:
  - `dep = { workspace = true }`
- No versions in subcrates. To change a version, update only `[workspace.dependencies]`.

### 1.3 Prohibitions
- Do not add "temporary" dependencies without real need.
- Do not pull heavy dependencies for a small utility (evaluate cost first).
- Do not duplicate the same dependency with different features across crates without a clear reason.

---

## 2) Code style (short and stable)

### 2.0 Imports and short names (no deep namespaces)
**Rule:** avoid "triple-nested" paths like `some_lib::some_mod::some_func` in code.

- All `use` imports must be **at the top of the file** (including `use some_lib::some_mod;`) — **do not place `use` inside functions/methods/blocks**.
- Prefer top-of-file imports and short, readable names in the body.
- Full paths are allowed only when they **clearly improve** readability (e.g., resolving name conflicts or single-use references).

### 2.1 Naming
- Choose the simplest and shortest names possible for functions/methods/types.
- Prefer standard, obvious words (`open`, `new`, `get`, `put`, `read`, `write`, `seek`, `stream`, `send`, `recv`) when they accurately reflect the meaning.
- Avoid "clever"/long names that encode internal implementation or refactoring history.

### 2.2 Comments and documentation
**Goal:** minimize stale comments; keep explanations close to interfaces.

- In-code comments — short, single-line only.
- Do not add walls of comments at the top of files.
- Do not write comments that restate the obvious.
- Anything requiring explanation (contracts, invariants, lifecycle, protocols, cache schemes) goes in the `README.md` of the corresponding subcrate.

### 2.3 File size and decomposition
**Goal:** Rust files should stay small to simplify navigation and reduce review cost.

- Do not let a single `.rs` file grow indefinitely.
- If a file starts becoming a "dump of abstractions", extract:
  - types into separate files,
  - implementations into `mod.rs` + `*.rs`,
  - policies/strategies into separate modules.
- Heuristics (what to extract):
  - large `enum`/`struct` + their `impl` blocks,
  - subsystems (e.g.: `lru_index`, `fs_layout`, `atomic_write`, `url_canon`, `key_processor`).

### Tests and fixtures: `src/` is for prod code only
**Rule:** `src/` must contain only code being tested (prod code). Test infrastructure must not leak into `src/`.

- If a test requires **substantial fixtures** (local server, large playlists/segments, content generators, request counters, complex scenarios) — these are **integration tests**:
  - Place them in `crates/<crate>/tests/` (e.g., `tests/fixture.rs`, `tests/integration_*.rs`);
  - Extract fixtures into `crates/<crate>/tests/fixture/` (submodules) if there are many.
- `src/` may only contain:
  - small unit tests next to the code,
  - very small test helpers under `#[cfg(test)]` that do not turn `src/` into a "fixture warehouse".
- Do not add "large" fixtures/servers to `src/` even under `#[cfg(test)]`.
- Criterion for "substantial fixtures": if it does not fit compactly into a single unit test or requires a separate struct/protocol — it belongs in `tests/`.


---

## 3) Test-driven development (TDD)

**Rule:** changes are driven by tests that describe desired behavior.

### 4.1 Workflow
1) Define behavior through test(s) (integration/unit — as appropriate).
2) Run tests and verify the new test fails for the expected reason.
3) Implement the minimal code to make the test pass.
4) Refactor only after all tests pass.

### 4.2 Test requirements
- Tests must be deterministic.
- Tests must not depend on external network.
- Tests must be reasonable in log/data volume (no reading gigabytes, no noisy dumps).
- Tests capture the contract, not accidental implementation details.

---

## 4) Generic programming (generics-first)

**Rule:** extensibility is achieved through generics and abstractions, not copy-paste and tight coupling.

### 4.1 Prefer standard and tokio abstractions
**Goal:** fewer reinventions, more ecosystem compatibility.
- Where possible, use standard and `tokio` traits/types and their idioms:
  - conversions (`From`, `TryFrom`, `Into`, `AsRef`),
  - errors (`std::error::Error`, `thiserror`),
  - iterators and adapters (`Iterator`, `IntoIterator`),
  - I/O traits (`Read`, `Seek`, `Write`, `AsyncRead`, `AsyncWrite` where appropriate),
  - `tokio` channels/synchronization (if the component is already on tokio) instead of custom solutions.
- If a standard trait fits, do not introduce a new "almost identical" trait.

### 4.2 Using generics
- Make base structures generic over:
  - event type,
  - source/transport type,
  - cache/storage type,
  - policy (ABR/eviction), etc.
- Extend behavior through type parameters, traits, and composition.

### 4.3 Prohibitions
- Do not create "nearly identical" structs for HLS/MP3 when differences can be expressed via generic/trait.
- Do not create "God traits" with dozens of methods — prefer several small ones.

---

## 5) Visibility and API surface

### 5.1 Default to `pub(crate)`
**Rule:** new items must be `pub(crate)` unless they are part of a documented public contract.
- Do not use bare `pub` for internal helpers, intermediate types, or implementation details.
- The `unreachable_pub` lint (enabled workspace-wide) will flag `pub` items that are not reachable from outside their crate.
- When promoting `pub(crate)` to `pub`, verify the item is tested and documented.

### 5.2 `#[non_exhaustive]` on public types
**Rule:** public enums and structs with named fields exposed across crate boundaries must be `#[non_exhaustive]`.
- This prevents downstream code (including AI-generated code in wrong crates) from constructing values directly.
- Forces use of constructors and builder patterns that can enforce invariants.
- Exceptions: small, stable types that are unlikely to gain new variants/fields (e.g., `VariantId(usize)`).

### 5.3 Single definition for shared types
**Rule:** shared types (`AudioCodec`, `ContainerFormat`, `MediaInfo`) live in `kithara-stream`. **Never** duplicate type definitions across crates.
- Before creating a new public type, search the workspace: `grep -r 'pub struct YourType\|pub enum YourType' crates/`.
- If a type exists, import it — do not create a parallel definition.
- `scripts/ci/check-arch.sh` enforces this automatically.

---

## 6) Loose coupling and modularity

**Goal:** components must be independent blocks that can be assembled into a system.

### 6.1 Separation of concerns
- Cache — separate component.
- Network/download — separate component.
- HLS orchestration — separate component.
- Decoding — separate component.

### 6.2 Inter-crate dependencies
- Avoid circular dependencies.
- Dependencies point "downward" toward base abstractions, never back up.
- Facade layer (if present) aggregates components but must not contain "smart" logic.
- `scripts/ci/check-arch.sh` validates dependency direction in CI.

### 6.3 Encapsulation
- External code must not know about internal cache details (paths/files/lease format).
- External code must not depend on a specific HTTP client unless it is part of the contract.

---

## 7) Linting/formatting and unified settings
**Goal:** uniform formatting and lint rules across the entire workspace.

- Agents must use and respect workspace configs:
  - `rustfmt.toml`
  - `clippy.toml`
  - `deny.toml`
- To change lint policy:
  - prefer changes in these files (single source of truth),
  - do not configure per-crate without a reason.
- To unify settings via `workspace.metadata`, do so in the root `Cargo.toml`, not in subcrates.

### Code Style Rules (enforced by `scripts/ci/lint-style.sh`)
- **No separator comments** (`// ====...`, `// ────...`) — use plain `//` section headers.
- **No inline qualified paths** — import everything at the top of the file via `use`; do not use `std::io::Error` in function bodies (write `io::Error`).
- **Struct fields ordered**: `pub` (alphabetical) → `pub(crate)` (alphabetical) → private (alphabetical). Exceptions: serde/bincode types with positional serialization.
- **`lib.rs`/`mod.rs`**: only `mod` declarations and `pub use` re-exports. Code and tests go in separate files.

---

## 8) General quality rules (brief)

- No `unwrap()`/`expect()` in prod code without a very strong justification.
- Errors must be typed and include context (what was being done, with which resource).
- Logs — minimal and useful; never log secrets (keys/tokens/key bytes).
- **Logging (normative):**
  - Use `tracing` (`trace!`, `debug!`, `info!`, `warn!`, `error!`) with appropriate levels instead of `println!` / `print!` / `dbg!`;
  - Add context via fields (e.g.: `asset_id`, `url`, `resource`, `variant`, `segment_idx`, `bytes`, `attempt`, `timeout_ms`);
  - Do not leave "temporary" prints in prod code (acceptable sparingly in tests, but `tracing` is still preferred).
- Any changes to public APIs must be accompanied by tests that capture the contract.

### Naming
- Choose the simplest and shortest names possible for functions/methods/types.
- Prefer standard, obvious words (`open`, `new`, `get`, `put`, `read`, `write`, `seek`, `stream`, `send`, `recv`) when they accurately reflect the meaning.
- Avoid "clever"/long names that encode internal implementation or refactoring history.

---

## 9) Resolving rule conflicts

- If a product requirement conflicts with these rules — discuss a compromise first and update `AGENTS.md`.
- Any forced rule bypass must be explained in the `README` of the corresponding subcrate (brief, to the point).
