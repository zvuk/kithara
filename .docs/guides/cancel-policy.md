# Cancel Policy

Open this only when touching cancellation.

Cancel is a typed propagate-down tree of `Arc<Node>` in
`kithara-platform/common/cancel/`. `CancelToken` is a handle into that tree:

- `Clone` keeps identity, pointing at the same node.
- `child()` derives a descendant.
- `cancel()` cancels the token's own subtree; epoch and per-fetch cancels are
  legitimate.
- `is_cancelled()` is one Acquire-load.
- `cancelled()` is async and `select!`-safe.
- `on_cancel()` is the sync waker seam.

Only two constructors mint a fresh tree root instead of deriving from a parent:

- `root()`: the owning master a subsystem holds and cancels on teardown.
- `never()`: the structurally required never-cancelled sentinel.

`CancelGroup` is the OR-combinator over a set of tokens.

`CancelScope::new(Option<CancelToken>)` is the canonical seam:

- passed parent -> `token = parent.child()`;
- `None` -> the scope's token is a fresh standalone `root()`.

`CancelScope::Drop` is passive. Teardown is an explicit `scope.cancel()` on the
owner's own subtree, never an implicit cancel of a potentially foreign master.

Hard-coded `CancelToken::root()` and `CancelToken::never()` are forbidden in
production code outside the allowlist enforced by `cargo xtask lint arch`
(`cancel_root_sites`):

- consumer-crate tops in `kithara-app` / `kithara-ffi`;
- `CancelScope` in `kithara-platform`;
- the `kithara-stream` batch `never()` sentinel;
- the `kithara-hls` `wake_signal` latch.

See `crates/kithara-play/CONTEXT.md` "Cancel Hierarchy" for the full runtime
contract.
