# Dedicated CI host

The Mac mini is a CI-owned machine. Repository automation has one executable
owner, `xtask ci`; GitLab YAML and `just` recipes only select a typed command.

`.config/ci-pins.toml` is the repository's, tracked and reviewed with the code
it pins; the host profile is the machine's and untracked. Neither repeats the
other. Lanes reach the profile only through `KITHARA_CI_HOST_CONFIG`, set by
every executor to `/etc/kithara-ci/mac-host.toml` (`C:/KitharaCI/mac-host.toml`
on Windows).

## Host installation

Write the machine profile first, outside the repository; its fields are in
`xtask/tests/fixtures/ci-mac-host.toml` (`ci-linux-host.toml` for Linux).

A Linux profile lists each repository served with its own token file, and every
runner names one. A GitHub registration reaches exactly one repository, so
repositories are peers with no default. A runner naming an uncredentialed
repository is refused at profile read; otherwise it registers against whatever
token is present and reports to the wrong repository. Write each token with
`install -m 600 -o root -g root /dev/stdin`: `sudo` overrides an inherited
umask, so `tee` leaves it world-readable.

`xtask ci host` owns the procedure. From a reviewed GitLab commit, with
`KITHARA_CI_HOST_CONFIG` exported and `sudo -E` where root is needed, run
`bootstrap`, `install-host-tools`, `finish`; `finish` installs binary, profile
and pins under `/Volumes/KitharaCI/services` and publishes the profile where the
lanes read it. Run the rest in the logged-in `kithara-ci` GUI session, against
those installed copies.

The Linux image is built from the pins alone: `RUST_VERSION` and
`RUST_BASE_DIGEST` select the base, every tool version arrives as a build
argument. A new tag is not deployed until this host rebuilds the image and
reruns `configure-runners` and `activate` from a checkout of the commit carrying
the pin. The runner never pulls this local-only tag and declares the tag it
provisioned, which `xtask ci run` checks against the pin.

## GitLab runners

One `glrt-...` project runner token per file, mode `0600`, at
`~/.config/kithara-ci/runner-<name>.token` for `macos`, `linux`, `android` and
`release`, tagged to match each lane's `tags:`; keep the release runner
protected. `configure-runners` writes the executors, Docker for Linux and host
shell for the rest, so the Apple lane reuses host filesystem and cache roots
across jobs, not a machine per build.

Apple packaging needs a case-folding checkout filesystem: Xcode creates a
`Headers` directory and `cargo-swift` addresses it as `headers`. When
`host_root` is case-sensitive, point `build_root` at a case-folding APFS
location; every runner uses it for `builds_dir`.

Runners and the bridge validate `gitlab_url` against the platform trust store;
no private CA is installed. A host that cannot build that chain is a network
fault to fix upstream.

The runner's launch agent uses launchd's `Interactive` process type and host
shell jobs inherit that scheduling policy, so marking the parent `Background`
throttles Cargo and the single-threaded source linters. Colima stays background;
Linux work has its own container CPU limit.

## Windows

`xtask ci host` provisions the UTM guest under `<host_root>/vm/windows` from the
profile, which owns its disk sizes; media and license are deliberately not
automated. Install the official GitLab Runner in the Windows 11 ARM guest by
hand: one shell executor, tag `kithara-windows`, `concurrent = 1`, builds and
cache under `C:\KitharaCI`. Its job runs `xtask ci run windows`; no PowerShell
script. Windows runs last in the nightly chain.

## Repository bridge

Copy `.config/bridge/config.example.toml` to
`/Volumes/KitharaCI/services/bridge/config.toml`; it and the two tokens belong
to UID 504 (`kithara-sync`), mode `0600`. The GitHub token needs
`Contents: write`, `Pull requests: read` and `Commit statuses: write`. Validate
with `ci bridge validate` (no network mutation), then `ci host activate-bridge`.
`github_branch` and `gitlab_branch` are separate keys because the sides
disagree — GitLab `develop`, GitHub `main` — and a swap is silent: GitHub
answers an unknown base with an empty pull list.

The daemon keeps running the executable that was installed, not the one on
`develop`: a fix changes nothing until `ci host install-services` reinstalls it
from a reviewed GitLab commit, then `activate-bridge` and `activate`. launchd
keeps the definition it loaded, so skipping `activate` silently leaves the
maintenance agents on the old cadence; `launchctl print` reports what is loaded.

The bridge moves either default branch only by fast-forward, in whichever
direction is behind, and never synthesizes a replacement commit or force-pushes
a diverged branch.

GitHub pull requests are verified before merge. While both default branches are
equal, the bridge reserves the exact head and base pair, publishes one
quarantine ref, and starts its GitLab pipeline; the result lands on the head
commit under the status context `kithara/gitlab-verification`. Branch protection
must require that context on `main` and forbid direct pushes and bypasses, or
the verifier is advisory. Once the default branch moves, the next attempt
reserves against the new base with a new ref.

A pull request changing a CI control path is rejected before a pipeline exists;
port it through a GitLab merge request: the code judging pull requests changes
under GitLab review.

A pipeline is judged on the child the dispatch stage triggers, never its parent,
which reports `success` over a cancelled child. Divergence is fail-closed and
opens one deduplicated GitLab incident. A rejection is recorded for the exact
head and base pair and refused on sight; `ci bridge retry` is the only route to
a rejudgement.

## The verdict

Gating on green would hold every change behind red it did not cause. The judged
lanes carry `allow_failure: true` and one job decides: a run is held for failing
something the default branch is not.

Each lane leaves what it produced in `.ci-artifacts/junit/`, collected by the
lane dispatcher, not the lane: the build directory survives between jobs, so the
report a lane is expected to write is removed before it runs. A lane that can
name no test leaves a marker naming itself, because GitLab hands a job no status
for the jobs it needed.

Every executor resolves the journal at
`<shared cache root>/verdict/journal.json` from its runner environment, so Linux
containers and macOS shell jobs share one baseline. `main` and the nightly chain
record; branch, merge-request and quarantine runs check against a window
unioning the last five recorded runs, so an intermittent failure is not read as
a regression.

## Storage policy

Profile thresholds are bytes used against the quota; cleanup takes each as the
free space to keep. On an APFS container shared with other volumes those differ:
read used-against-quota as free space and a volume reports `Normal` while jobs
are already refused.

The profile owns this policy: `removable_roots` names the trees cleanup takes
whole, `active_lease_hours` how long a cache lease keeps one alive,
`log_limit_bytes` when a log rotates. Individual Cargo, Gradle and sccache files
are never deleted in place; sccache keeps its own LRU limit. No `diskutil apfs`
verb accepts `-quota` after creation, so the quota cannot be raised and cleanup
is the whole answer. What its sweeps cannot show:

- The Linux guest's `/var/lib/docker` data disk is not mounted `discard` as its
  root is, so deleted layers stay allocated in a sparse file this volume pays
  for; every cleanup trims the `colima_profile` instance, whatever the pressure.
- A cache namespace that stops being written to goes invisible rather than
  stale, leaving a retired tool's store behind; `cache_namespaces` lists the
  live ones and cleanup takes the rest whole.
- Build-cache bytes for a lane's claimed checkout still count against the
  ceiling.
- A macOS job VM clone outlives the runner that cloned it, and age alone cannot
  prune its bundle without taking the base bundle, which only
  `tart create --from-ipsw` and a person can rebuild. It goes once tart reports
  it stopped *and* untouched for a day: a boot outlasts the cleanup interval and
  an idle booted guest writes nothing for hours, so neither alone separates idle
  from dead. `tart clone` copies on write, so a walk overstates what deleting one
  returns.
- `CiEnvironment` points every lane's `TMPDIR` under `/tmp/kithara-ci`: outside
  the checkout and both CI roots, short enough for the Unix sockets the suite
  binds, on storage the macOS guest can bind. Killed jobs leak there steadily
  and are pruned on age alone, an `lsof` walk per candidate costing hours over
  that backlog.

Health and cleanup run through launchd, and directly as `ci host health` /
`ci host cleanup`. A `KeepAlive` agent that dies on startup stays loaded and is
restarted forever, so health checks each `always_on_agents` process, not the
loaded service: a missing one looks like nothing from outside, its jobs sitting
`pending` while the pipeline reads as hung.

## GitLab project settings

Protect `develop`, release tags, the `release` environment, and the runner,
release and bridge credentials. Keep release publication manual and restricted
to maintainers. Give the nightly and weekly schedules the kind variable
`.gitlab-ci.yml` reads.

### Renaming the default branch

Pipeline rules read `$CI_DEFAULT_BRANCH`, so the repository needs no edit; what
follows the name is outside it, and the order matters:

1. Create the branch, then move the default-branch setting — until it moves, a
   push to the new branch dispatches as the `branch` kind, not the `main`
   kind.
2. Protect it before deleting the old one: protected variables reach protected
   branches only, so release jobs fail on a missing secret instead.
3. Repoint the schedules; a schedule keeps the branch it was created with.
4. Retarget open merge requests, which GitLab closes with their target branch.
5. Set `gitlab_branch` on the host, then `ci host activate-bridge`.
6. Delete the old branch last.
