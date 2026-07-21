# Agent Hooks

Use this guide only when touching tool adapters, hook behavior, or command
routing. Hooks are workflow guards, not code-style policy. Architecture and
Rust shape are still enforced by the repository lint recipes.

## Execution Route

Claude and Codex pass their hook JSON unchanged on stdin to one repo-owned
entry point:

```text
tool adapter -> just agent-hook
             -> _xtask-cached optional agent-hook
             -> cached xtask
```

The adapters only verify that `just` is available. They do not find the
repository root, inspect agent-specific project-directory variables, invoke
Cargo, or select a hook subcommand. Just's parent-directory lookup selects the
nearest repository `justfile`, and the payload identifies the event and tool.

`agent-hook` uses the generic cached xtask transport in `optional` mode. A
missing or unusable transport prints a warning and skips the guard. Once the
Rust hook starts, a policy denial, malformed payload, invalid configuration, or
formatter failure is propagated to the calling agent instead of failing open.

There is no hook install command. Prime or refresh the shared xtask cache with:

```sh
just xtask --help
just xtask-refresh
```

The first command bootstraps only when the cache is absent and refreshes only
when it is stale. The second forces a refresh.

## Payload Routing

`agent-hook` has no `pre-bash` or `post-edit` CLI discriminator. It parses
`hook_event_name`, `tool_name`, and `tool_input` from stdin, maps them to typed
event and tool kinds, and selects a configured handler. Handler routes and the
destructive-Git override variable live in `.config/xtask.toml`:

```toml
[ext.agent_hook]
destructive_git_override_env = "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT"

[[ext.agent_hook.routes]]
event = "pre-tool-use"
tool_kind = "shell"
handler = "command-guard"

[[ext.agent_hook.routes]]
event = "post-tool-use"
tool_kind = "file-edit"
handler = "format-edited-paths"
```

Adapter matchers decide which tool events reach the hook. Claude sends
`PreToolUse` for `Bash` and `PostToolUse` for `Write|Edit|MultiEdit`; Codex sends
`PreToolUse` for `Bash` and `PostToolUse` for `apply_patch`. Adding another
supported event or tool requires a typed payload mapping and a route, not a new
launcher argument.

## Command Guard

The `command-guard` handler denies common expensive or destructive command
mistakes:

- broad raw test acceptance, such as unfiltered workspace `cargo test` or
  `cargo nextest run`; use `just test`;
- direct formatter gates that bypass the repository harness; use `just fmt` or
  `just fmt-check`;
- an outer timeout around the full test harness;
- destructive Git commands such as `git reset --hard`, `git clean`, or
  `git checkout -- ...`.

The destructive-Git override variable is configured in `.config/xtask.toml`
and still requires explicit user approval. Scoped Cargo probes remain allowed.

## Edited-Path Formatting

The `format-edited-paths` handler formats only paths reported by the hook:

- `.rs` uses nightly `rustfmt` with child-module traversal disabled;
- non-Cargo `.toml` uses Taplo;
- `.json` and `.jsonc` use `tidy-json`.

`Cargo.toml` is deliberately skipped because its canonical dependency-order
rewrite is workspace-wide. Run `just sort` explicitly. Unknown file types are
ignored. Formatter commands and path containment are owned by
`kithara-devtools`; the hook does not duplicate formatter flags or scan the
workspace.

## Shared xtask Cache

Agent hooks use the same self-cache as every Just recipe that invokes xtask.
The generic recipe graph is:

```text
just xtask <args> -> _xtask-ready -> current: cached run
                                  -> stale: cached single-flight refresh
                                  -> missing: Cargo bootstrap
                    _xtask-cached strict <args>
```

The ignored `xtask/.xtask-cache` locator contains one absolute path to an
immutable generation below the concrete worktree Git directory. A warm command
reads that locator and starts the binary directly; it does not start Cargo or
Git. Cargo is used only for a cold bootstrap or stale refresh, with its private
target retained at `target/xtask-self-cache`.

The Just recipes only resolve the locator, ensure a current generation, and
execute it. Rust owns freshness, single-flight locking, publication, leases,
and bounded generation cleanup. Cache policy is typed under `[ext.xtask.cache]` in
`.config/xtask.toml`; it controls declared extra inputs, retained generations,
and the cleanup grace period. Hook route changes are runtime policy and do not
by themselves make the binary stale.

Hooks never refresh and never wait for the refresh lock. They run the active
last-good generation while a normal xtask invocation refreshes in parallel.
Publication replaces the locator only after the new immutable generation is
complete, so a failed build leaves the active generation untouched. The
builder only returns a Cargo artifact; it cannot publish. Its supervisor
terminates and reaps the Cargo process group if the refresh parent exits. The
publisher compares source identities from before and after the build and
refuses activation if an input changed in between.

The former `xtask/.agent-hook-cache` locator stays ignored for a smooth
checkout transition, but the new transport never reads it. Its
`<git-dir>/kithara-agent-hook` generations are legacy artifacts and can be
removed after older agent processes have exited.

## Tool Adapter Rule

Tool-specific JSON files contain only event matchers and the small command that
delegates to `agent-hook`. Keep policy, cache ownership, root discovery, and
formatter behavior out of adapters.
