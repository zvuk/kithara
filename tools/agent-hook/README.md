# kithara-agent-hook

Repo-owned policy binary for agent shell and edit hooks. Tool adapters invoke it
through `tools/agent-hook/run`; the compatibility `xtask agent-hook` command uses
the same library. Behavior and launcher invariants are documented in
[`docs/guides/agent-hooks.md`](../../docs/guides/agent-hooks.md).
