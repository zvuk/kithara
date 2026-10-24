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

`xtask ci host mac` owns the procedure. From a reviewed GitLab commit, with
`KITHARA_CI_HOST_CONFIG` exported and `sudo -E` where root is needed, run
`bootstrap`, `install-host-tools`, `finish`; `finish` installs binary, profile
and pins under `/Volumes/KitharaCI/services` and publishes the profile where the
lanes read it. Run the rest in the logged-in `kithara-ci` GUI session, against
those installed copies.

The Linux image is built from the pins alone: `RUST_VERSION` and
`RUST_BASE_DIGEST` select the base, every tool version arrives as a build
argument. The runners name the floating tag — `kithara-ci:linux-latest` and
its siblings — never the pin, so a pin bump cannot strand a lane on an image
nobody built: the pin says what to build, the floating tag says what to run,
and whatever builds an image moves the tag onto it.

Declared lanes select the checkout's nextest pin in child `PATH` from a
versioned cache scoped to trust and host platform. A cache miss installs it as
the CI user into that cache while host setup retains ownership of global tools
and privileged fleet provisioning.

## Provisioning

`ci host provision` is the roll-out, run on the machine it provisions and
reading everything from the commit it runs on. The macOS host reinstalls its
services, rewrites its runner configuration, makes sure the pinned Linux image
is present and reloads its agents; a Linux host builds the images its profile
asks for, creates every cache mount its runners use and hands it to the job
user, and installs its units. Every step is idempotent — an image that is
already there is only retagged — so a run costs seconds when nothing moved.

Neither pipeline runs it on a push. On GitLab it is `host:provision`, reached
only by a pipeline started on the default branch with `KITHARA_PROVISION=1`;
that run carries nothing else. On GitHub it is the `Host` workflow, started by
hand. Provisioning writes to the machine
every lane depends on, and a merge-request or quarantine ref carries code no
one has reviewed yet.

Three things are granted once, by hand, and nothing in a pipeline can grant
them:

- A non-ephemeral runner on the Linux host itself, labelled by
  `KITHARA_HOST_RUNNER_LABEL`, with the Docker daemon and `systemctl` in
  reach. The fleet's own runners are throwaway containers that have neither.
- The steps that own root-owned files re-run this executable under `sudo -n`,
  which never prompts. A line in `/etc/sudoers.d/kithara-ci` granting the
  runner user that one command — and nothing else — is what lets them run;
  the step's own failure message spells it out.
- A Rust toolchain on the provisioning host, because the pass is this
  executable built from the checkout.

## GitLab runners

One `glrt-...` project runner token per file, mode `0600`, at
`~/.config/kithara-ci/runner-<name>.token` for `macos`, `linux`, `android` and
`release`, tagged to match each lane's `tags:`; keep the release runner
protected. `configure-runners` writes the executors, Docker for Linux and host
shell for the rest, so the Apple lane reuses host filesystem and cache roots
across jobs, not a machine per build.

Set `build_root` when checkouts need a separate storage location from
`host_root`; every runner uses it for `builds_dir`.

Runners and the bridge validate `gitlab_url` against the platform trust store;
no private CA is installed. A host that cannot build that chain is a network
fault to fix upstream.

## Apple USDT observer

`apple:usdt` is required on every branch and review pipeline. It runs an
external observer with `sudo -n /usr/sbin/dtrace`, attaches it to a same-user
child, and parses the raw `kithara:::probe_0` through `probe_5` records. This
is the acceptance boundary for the provider ABI; an in-process collector does
not prove that DTrace can discover or read the probes.

The same job runs the feature-gated product contracts, including HLS seek
stress, under the external-observer feature closure.

The `kithara-ci` runner account needs a narrowly scoped passwordless sudoers
entry for `/usr/sbin/dtrace`, and macOS must permit DTrace attachment to a
same-user process. Install or upgrade the runner only after this succeeds in
the logged-in `kithara-ci` session:

```sh
sudo -n /usr/sbin/dtrace -q -n 'syscall:::entry { exit(0); }' -c '/bin/true'
```

If SIP, Developer Mode, or the runner's entitlement blocks that command, fix
the host policy. Do not mark the lane optional or replace it with tracing
capture.

The runner's launch agent uses launchd's `Interactive` process type and host
shell jobs inherit that scheduling policy, so marking the parent `Background`
throttles Cargo and the single-threaded source linters. Colima stays background;
Linux work has its own container CPU limit.

## Pipeline scheduling

Dispatch waits for its child's verdict without holding a host resource group.
Independent pipelines can therefore fill the runner's available slots. Measured
child jobs retain `kithara-suite`; the runner limits total parallelism, each job
owns its checkout and compiler-cache slot, and the verdict journal locks its
read-modify-write transaction. Raising the runner limit is a separate host-capacity
change, not a prerequisite for removing idle time between pipelines.

Native lint, tests and verdict jobs use the shared `kithara-macos` tag so either
native registration can take them. Each registration retains its own checkout,
build root and compiler-cache socket. Device builds retain `kithara-macos-2`;
measured jobs still serialize through `kithara-suite`.

Host-shell registrations run the installed host preflight before fetching sources.
Critical disk pressure refuses the job before checkout or compilation can spend
the remaining space. Provisioning installs the executable, host profile and pins
together; the admission hook uses that installed set.

Superseded branch and merge-request child pipelines cancel interruptible checks,
including iOS and end-to-end review suites. Main, scheduled, release, and explicit
platform runs retain their cancellation policy. Quarantine refs are unique per
head/base pair; the bridge retires obsolete attempts rather than relying on
same-ref push cancellation.

## Windows

`xtask ci host mac` provisions the UTM guest under `<host_root>/vm/windows`
from the profile, which owns its disk sizes; media and license are deliberately
not automated. Install the official GitLab Runner in the Windows 11 ARM guest
by hand: one shell executor, tag `kithara-windows`, `concurrent = 1`, builds
and cache under `C:\KitharaCI`. Its job runs `xtask ci run windows`; no
PowerShell script. Windows runs last in the nightly chain.

## Repository bridge

See [CI bridge and verdict](ci-bridge.md#repository-bridge) for repository
synchronization, PR status publication and bridge deployment.

## The verdict

See [CI bridge and verdict](ci-bridge.md#the-verdict) for lane reports and the
shared regression journal.

## Object cache service

Linux CI containers leave compile admission to their CPU and memory limits.
The root `justfile` disables kache's store-wide scheduler for configured Linux
CI runners before bootstrapping xtask. Kache sizes that shared pool from the
calling container's CPU count, so independent containers would otherwise
compete for one container's slots. Other invocations retain their scheduler
setting, enabled by default.

Each host runs its own RustFS stack, the compose project `kithara-ci-cache`
published on `127.0.0.1:19000`; the two hosts share no cache. Jobs reach it
through the `rc` client.
`docker/ci-cache.compose.yml` declares it, and
`docker/ci-cache/linux.env.example` and `docker/ci-cache/macos.env.example`
give the shape of the environment each is started with. The environment itself
lives on the host, outside the repository, because it names directories and
quotas of that machine.

Everything the stack keeps - the admin credentials, the objects, the client
keys - sits in a directory on the host's disk that the environment names. A
named volume lives wherever the Docker daemon keeps it, inside colima's
virtual machine on the Mac, so a reset of that daemon would take the store and
the admin password with it, and every client key the runners hold would stop
working.

The server needs RustFS 1.0.1 or later. 1.0.0 answers a bucket quota request
with 503 for about ten seconds after it reports ready (rustfs/rustfs#8014), so
a setup started on a fresh stack fails at its first quota request.

The stack is one container. Its image carries RustFS, `rc` and `xtask`, and
runs `xtask ci cache serve`: it writes the administrator's credentials if
there are none, starts RustFS as its child, waits for it to answer ready,
creates each scope's bucket, lifecycle and client key, and then runs the
evictor beside the store. They start and stop together: a stop signal is
passed to RustFS and waited on, and when either RustFS or the evictor ends,
the container ends and Docker restarts it. Every start applies the setup
again, from the code the store is served with. `serve` builds every bucket
policy from `ci::cache::provision`. So the copy of this repository the image
was built from, not the repository itself, decides what the live policy says.
The Linux host's deployment copy is `/etc/kithara-ci/cache-compose/source`;
refreshing it is copying a tree over that path, keeping the one it replaces as
`source.before-<stamp>` beside it, and rebuilding the image from it.

Which copy a running stack was actually started from is a question to ask the
stack, not this page:

```
docker inspect kithara-ci-cache \
  --format '{{index .Config.Labels "com.docker.compose.project.working_dir"}}'
```

On 2026-09-23 it answered `/mnt/sdb1/kithara-ci/worktrees/pr362-proof-44320e421`

- a checkout a pull request had left behind, two weeks stale - so refreshing the
  deployment copy had been changing nothing the server ran. The stack was
  recreated from the deployment copy with its image rebuilt, and the environment
  it is started with now lives beside it as `linux.env`.

A policy added in the repository therefore does not reach the server by being
merged. `source-snapshots/` was added to the non-trusted statement on
2026-09-22 while the host still carried its copy from 2026-09-09: every branch
scope was refused the listing of the dependency source layer, and every branch
job fetched its dependencies from the public internet instead. The layer was
there the whole time. The refusal named the bucket with an empty key, which is
what a `ListBucket` denial always looks like - so it read as a broken client
rather than a policy that had never been updated.

Quotas are per scope, written in the host's environment file as a whole number
of KiB, MiB, GiB or TiB. The evictor reads them when the stack starts and keeps
each bucket under its own, so a quota changes in the file and takes effect at
the next start. The store itself carries none: every start clears any quota a
bucket has. With a quota, RustFS 1.0.1 passes every write to the bucket through
one lock and refuses a write that has waited five seconds for it with 503.
Under the fleet's load that was about ten thousand refused writes a day on the
Linux store, measured on 2026-10-06; each cost the writer a retry. A quota set
by hand with `rc bucket quota set` brings that back until the next start.

The two drift, and the drift is the danger: the live buckets had been raised by
hand to 200 GiB trusted and 800 GiB review while the environment the stack was
started with still said 50, and the per-fork scopes existed outside the
`CACHE_SCOPES` it named - so a setup run would have flattened every quota
and known nothing of half the buckets. A scope that needs
its own size now names it, `CACHE_BUCKET_QUOTA_<SCOPE>`, and
`CACHE_BUCKET_QUOTA` is what the scopes that say nothing are given. The
environment on the host states what the buckets actually are, so applying it is
no longer a way to lose them.

That holds only if the setup sees the whole environment. Compose hands a
container only the variables its file names, and `--env-file` only fills in
the Compose file itself. Until 2026-10-02 the service named four variables, so
a per-scope quota never reached it, and every setup set each scope to
`CACHE_BUCKET_QUOTA`. `just ci cache` now passes the file's absolute path as
`CACHE_ENV_FILE`, and the service reads it as its `env_file`. A copy of the
source older than that still flattens the quotas: refresh it before starting
the stack.

The compiler cache has no age rule: a hit does not renew an object's date, so
an age rule expires first the entries every build reads. The evictor keeps
each scope's compiler cache under its quota by last use instead. The store
posts its audit log to it, one record per request, on the container's
loopback: the evictor takes the log on trust, and every job shares the network
the store is published on. RustFS refuses to send to loopback unless the
origin is allowed, so the stack allows exactly that one. A successful read or
write of an entry is a use. Every half hour it lists each scope, and once the
bucket holds four fifths of its quota it removes the entries used longest ago
until it is under thirteen twentieths. Each finished pass writes the last
reads to the `ci-cache-recency` bucket, one object per scope, so a restart of
the evictor loses only the reads since; a record that does not read back is
set aside. Either only makes entries look older than they are.

Nothing else bounds a bucket. Between passes it can run past its quota by
what the fleet writes in half an hour, and a pass that fails leaves it there
until the next one; the disk is the last limit. The evictor and the store stop
together, so a store that serves is a store being evicted. What the store
could not deliver to the evictor waits in its memory up to the queue limit; a
restart of the store loses it. A lost read only makes an entry look older than
it is. `docker logs kithara-ci-cache` prints one line per pass among the
store's own lines.

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
is the whole answer.

`build_cache_size` caps one cache and cannot see whether the volume has room:
two checkouts each under a 100 GB cap held 183 GB between them while every pass
reported nothing freed and jobs were already refused. Warm builds are the last
rung: when stale trees, the Docker build cache and the guest trim still leave
the volume at `Aggressive` or worse, the pass reclaims what it is short of the
soft floor, evicting past the cap. What its sweeps cannot show:

- The Linux guest's `/var/lib/docker` data disk is not mounted `discard` as its
  root is, so deleted layers stay allocated in a sparse file this volume pays
  for; every cleanup holds Docker's build cache to `docker_build_cache_size` and
  trims the `colima_profile` instance, whatever the pressure.
- A cache namespace that stops being written to goes invisible rather than
  stale, leaving a retired tool's store behind; `cache_namespaces` lists the
  live ones and cleanup takes the rest whole. A namespace with an owner belongs
  there even when it is quiet: `cargo` holds the Linux guest's Cargo home,
  which a job fetches into only when its lockfile changes, and `kache` holds
  kache's host files and the store of compiles no job placed.
  An installed profile carries the list verbatim, so adding a name reaches a
  running host only by editing its `/etc/kithara-ci/mac-host.toml` as well.
- Build-cache bytes for a lane's leased build directory still count against
  the ceiling.
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

Automatic cleanup preserves the shared Colima VM, its Docker data disk and the
services running inside it. If safe cache reclamation cannot restore the free-space
floor, cleanup reports failure and preflight refuses new jobs. Runtime deletion
is an explicit maintenance operation, never a disk-pressure cleanup step.

Health and cleanup run through launchd, and directly as `ci host mac health` /
`ci host mac cleanup`. A `KeepAlive` agent that dies on startup stays loaded
and is restarted forever, so health checks each `always_on_agents` process, not
the loaded service: a missing one looks like nothing from outside, its jobs
sitting `pending` while the pipeline reads as hung.

launchd starts no second instance while the first is alive, so a wedged pass
silences `StartInterval` outright: one hung for over a day inside `opendir` on a
volume that had stopped answering, and nothing said so. A watchdog thread ends
the process at `cleanup_deadline_seconds`, thirty minutes by default, so the
next tick gets a machine that can try again. It cannot be a check between steps,
because such a pass never reaches the next step.

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
1. Protect it before deleting the old one: protected variables reach protected
  branches only, so release jobs fail on a missing secret instead.
1. Repoint the schedules; a schedule keeps the branch it was created with.
1. Retarget open merge requests, which GitLab closes with their target branch.
1. Set `gitlab_branch` on the host, then `ci host mac activate-bridge`.
1. Delete the old branch last.
