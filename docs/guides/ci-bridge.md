# CI bridge and verdict

## Repository bridge

The macOS host profile supplied with `ci host mac --config` defines the
top-level `host_root` key. `<host_root>` below denotes that configured installation
root; it is not a fixed volume name.

Copy `.config/bridge/config.example.toml` to
`<host_root>/services/bridge/config.toml`; it and the two tokens belong
to the profile's `sync_user` (`sync_uid`), mode `0600`. The GitHub token needs
`Contents: write`, `Pull requests: read`, `Actions: read`, `Checks: read` and
`Commit statuses: write`. Reading Actions, checks and commit statuses must also
cover each contributing repository whose CI is mirrored.
Validate with `ci bridge validate` (no network mutation), then
`ci host mac activate-bridge`.
`github_branch` and `gitlab_branch` are separate keys because the sides
disagree — GitLab `develop`, GitHub `main` — and a swap is silent: GitHub
answers an unknown base with an empty pull list.

The daemon runs the installed executable. Merging a fix into `develop` or
`main` does not replace it. Initial installation and service/profile changes use
`ci host mac install-services` from a reviewed commit, followed by loading the
changed launchd definitions. `launchctl print` reports the loaded definition.

For a binary-only update, the CI user can replace
`<host_root>/toolchains/shared-bin/kithara-ci`, which the bridge executes
through `/usr/bin/env` on each tick. Retain the previous binary, verify the
reviewed artifact's source revision and checksum, transfer it beside the active
copy, and replace that copy atomically after verifying the transferred checksum.
Validate with `ci bridge validate` as the sync user through the existing sudo
grant, then run one reconciliation and compare the live PR statuses with their
source jobs. If validation fails, restore the retained binary atomically.
This path does not require replacing the root-owned `services/bin` copy or
restarting runners. Changing service arguments or cadence still requires loading
the changed launchd definition.

The bridge moves either default branch only by fast-forward, in whichever
direction is behind, and never synthesizes a replacement commit or force-pushes
a diverged branch.

On each open pull-request head the bridge reads the source repository named by
the PR, without a configured list of fork owners or runner hosts. Its Actions
jobs, commit statuses and check runs appear under `GitHub /`, with
`kithara/github-ci` summarizing the reported source checks. External CI servers
must publish their results to that source repository and exact commit. The
standard `ci.yml`, when observed, still requires an executed successful
`CI required`. The exact GitLab
verification's parent and expected child jobs appear under `GitLab /`. Each
status retains its supplied source link and diagnostic text. An external CI
must publish separate signals for separate jobs; unpublished private jobs cannot
be discovered, and missing links or explanations remain diagnostic gaps.
Jobs retain their actual results, including
failures GitLab's verdict allows. The existing `kithara/gitlab-verification`
remains the verdict for the merge against its recorded base. Missing runs,
active retries and unreadable sources cannot reuse a previous green summary.

`kithara/pr-size` reads its positive limit from
`[ext.ci].pr_review_line_limit` in `.config/xtask.toml` at the current trusted
target commit. The checked-in value is 5000. A PR cannot raise its own limit,
and changing the target's configuration takes effect on the next observation.
Missing or invalid policy produces an error status. Review lines are added text
lines in the pull-request diff from its merge-base. Deleted lines and unchanged
file moves cost zero;
edits to moved files count their additions. All text files count, and binary
changes have no line count. Git computes this from fetched objects, independently
of truncated pull-request file APIs. These statuses become merge requirements
only when the repository's branch protection or ruleset requires them.

Require `kithara/pr-size` on the configured target to enforce the limit; preserve
all existing host-summary requirements and unrelated protections. Status
publication alone is not merge enforcement. Rules administration and bridge
installation are separate privileges, and a missing privilege leaves rollout
incomplete.

Before installing, record the reviewed commit, trusted target policy commit and
installed binary's hash, and retain the previous reviewed binary for rollback.
Use `install-services` and `activate-bridge` when the service definition or host
profile changes; binary-only updates use the shared-copy procedure above.
Validate the private bridge configuration without copying tokens into evidence.
Confirm the active launchd executable matches the installed artifact, then
verify linked job statuses on a live PR. `activate` also restarts Colima and
GitLab Runner, so coordinate it with active jobs rather than using it as a
bridge-only restart. To roll back, restore the previous reviewed artifact with
its matching trusted target policy and activate its bridge. Policy changes use
the normal target-branch review path. Keep the size requirement in place and
report rollout as incomplete until the required statuses are produced again.

The trusted target must contain the required positive review-limit setting
before activating an executable that reads it. A candidate PR's setting does
not supply target policy. Verify Actions, checks and status access for each
source repository, then compare exact-head projected results and supplied
failure details with their source. Local fixtures prove observation behavior;
the installed artifact and live statuses establish runtime acceptance.

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
