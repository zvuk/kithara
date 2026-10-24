# CI bridge and verdict

## Repository bridge

Copy `.config/bridge/config.example.toml` to
`/Volumes/KitharaCI/services/bridge/config.toml`; it and the two tokens belong
to UID 504 (`kithara-sync`), mode `0600`. The GitHub token needs
`Contents: write`, `Pull requests: read`, `Actions: read` and
`Commit statuses: write`. Actions read access must also cover each contributing
repository whose CI is mirrored.
Validate with `ci bridge validate` (no network mutation), then
`ci host mac activate-bridge`.
`github_branch` and `gitlab_branch` are separate keys because the sides
disagree — GitLab `develop`, GitHub `main` — and a swap is silent: GitHub
answers an unknown base with an empty pull list.

The daemon keeps running the executable that was installed, not the one on
`develop`: a fix changes nothing until `ci host mac install-services`
reinstalls it from a reviewed GitLab commit, then `activate-bridge` and
`activate`. launchd keeps the definition it loaded, so skipping `activate`
silently leaves the maintenance agents on the old cadence; `launchctl print`
reports what is loaded.

The bridge moves either default branch only by fast-forward, in whichever
direction is behind, and never synthesizes a replacement commit or force-pushes
a diverged branch.

On each open pull-request head the bridge publishes the fork's latest `ci.yml`
run as `kithara/github-ci` and its jobs under `GitHub /`; the exact GitLab
verification's parent and expected child jobs appear under `GitLab /`. Each
status links to its source job. Jobs retain their actual results, including
failures GitLab's verdict allows. The existing `kithara/gitlab-verification`
remains the verdict for the merge against its recorded base. Missing runs,
active retries and unreadable sources cannot reuse a previous green summary.

`kithara/pr-size` permits at most 5000 added text lines in the pull-request diff
from its merge-base. Deleted lines and unchanged file moves cost zero;
edits to moved files count their additions. All text files count, and binary
changes have no line count. Git computes this from fetched objects, independently
of truncated pull-request file APIs. These statuses become merge requirements
only when the repository's branch protection or ruleset requires them.

Require `kithara/pr-size` on the configured target to enforce the limit; preserve
all existing host-summary requirements and unrelated protections. Status
publication alone is not merge enforcement. Rules administration and bridge
installation are separate privileges, and a missing privilege leaves rollout
incomplete.

Before installing, record the reviewed commit and the installed binary's hash,
and retain the previous reviewed binary for rollback. Use `install-services`
and `activate-bridge` with the reviewed executable and existing host profile;
validate the private bridge configuration without copying tokens into evidence.
Confirm the active launchd executable matches the installed artifact, then
verify linked job statuses on a live PR. `activate` also restarts Colima and
GitLab Runner, so coordinate it with active jobs rather than using it as a
bridge-only restart. To roll back, reinstall the previous reviewed artifact and
activate its bridge; keep the size requirement in place and report rollout as
incomplete until the required statuses are produced again.

GitHub pull requests are verified before merge. While both default branches are
equal, the bridge reserves the exact head and base pair, publishes one
quarantine ref, and starts its GitLab pipeline; the result lands on the head
commit under the status context `kithara/gitlab-verification`. The verifier is
advisory by decision; merging stays the maintainer's call, so the status carries
the risk. Every verdict names the default-branch commit it was judged against.
When the default branch moves after a pass, the next tick sets the status to
pending and says the earlier pass was against an older base. Each default-branch
commit carries the verdict of its own GitLab pipeline under the same context;
a commit that stops being the head is still watched until its pipeline finishes.
Each status links to its pipeline. Once the default branch moves, the next attempt
reserves against the new base with a new ref.

A pipeline is judged on the child the dispatch stage triggers, never its parent,
which reports `success` over a cancelled child. Divergence is fail-closed and
opens one deduplicated GitLab incident. A rejection is recorded for the exact
head and base pair and refused on sight; `ci bridge retry` is the only route to
a rejudgement.

A stopped run is not a verdict. A queue emptied by hand, an auto-cancel or a
runner taken down leaves the branch unverified rather than rejected, so the
bridge releases the pipeline and opens the next attempt; the following tick
publishes a fresh ref and starts a run of its own. Only a run that reported
takes `ci bridge retry` to be judged again.

A verification branch is removed once nothing will name it again: its base has
moved, or no open pull request stands on its head. Its queued run is cancelled
first, because deleting the ref does not release the resource-group slot the run
is holding.

## The verdict

Judged lanes set `allow_failure: true`; the verdict holds only failures absent
from the default branch.

The lane dispatcher clears expected reports before execution because build
directories persist, then collects outputs in `.ci-artifacts/junit/`. A lane
that cannot name a test leaves its own failure marker: GitLab exposes no
needed-job status.

Branch's only test producer, `apple-test`, writes its canonical plan's selected
lane names to `.ci-artifacts/junit/apple-test.selection.json` before running it.
Only valid `[]` proves no suite applies. Nonempty lists need its own report
with test cases. Failure markers and invalid or failing XML retain their behavior;
other pipeline kinds require reports or failure markers.

Executors use their runner environment's `<shared cache root>/verdict/journal.json`
as one Linux/macOS baseline. `main` and nightly runs record; branch, merge-request
and quarantine runs check the last five recorded runs' union to absorb intermittent
failures.
