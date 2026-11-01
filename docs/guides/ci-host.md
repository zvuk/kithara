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

On the Linux host, the image is built from the pins alone: `RUST_VERSION` and
`RUST_BASE_DIGEST` select the base, every tool version arrives as a build
argument. The runners name the floating tag — the toolchain image and
its runner siblings — never the pin, so a pin bump cannot strand a lane on an image
nobody built: the pin says what to build, the floating tag says what to run,
and whatever builds an image moves the tag onto it.

Xcode follows the host installation rather than an exact repository version gate.
Every Apple lane reports its version and requires completed first-launch setup.
Target snapshot keys include the actual Xcode and macOS SDK builds; dependency
source snapshots remain independent of the Apple toolchain. After an Xcode
installation or update, finish its licence and first-launch components through
host setup before admitting Apple jobs.

Declared lanes select the checkout's nextest pin in child `PATH` from a
versioned cache scoped to trust and host platform. A cache miss installs it as
the CI user into that cache while host setup retains ownership of global tools
and privileged fleet provisioning.

## Provisioning

`ci host provision` is the roll-out, run on the machine it provisions and
reading everything from the commit it runs on. The macOS host reinstalls its
services, rewrites its native runner configuration and reloads its agents; a Linux host builds the images its profile
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
`~/.config/kithara-ci/runner-<name>.token` for `macos-1`, `macos-2`, `android` and
`release`, tagged to match each lane's `tags:`; keep the release runner
protected. `configure-runners` writes native shell executors. Apple and
dependency-audit lanes share the host filesystem and cache roots across jobs.
Linux and browser jobs run on the separate GitHub Linux fleet; the Mac has no
Linux runner, Docker image builder or Colima launch agent.

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
throttles Cargo and the single-threaded source linters.

## Pipeline scheduling

Dispatch waits for its child's verdict without holding a host resource group.
Independent pipelines can therefore fill the runner's available slots. Measured
child jobs retain `kithara-suite`; the runner limits total parallelism, each job
owns its checkout and compiler-cache slot, and the verdict journal locks its
read-modify-write transaction. Raising the runner limit is a separate host-capacity
change, not a prerequisite for removing idle time between pipelines.

Native lint, tests and verdict jobs use the shared `kithara-macos` tag so either
native registration can take them. Each registration retains its own checkout,
build root and compiler-cache socket. Framework packaging also uses the common
native pool; bindings, staging and Xcode DerivedData stay in each job's
checkout. Simulator/server work retains
`kithara-macos-2` through shutdown and device deletion; measured jobs still
serialize through `kithara-suite`.

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

Each independent host store needs a default-branch dependency-source publisher:
`linux-lint` on GitHub and `apple-lint` on the Mac's GitLab pipeline. Publication
uses the bucket named by the job's actual credentials; a claimed trust label
does not select another bucket. Branch and quarantine runs only restore the
layer. A main publisher fails if its upload fails. A main run on another host
does not fill this host's store.

Before its compiler daemon stops, a lane retains sanitized observations in
a job-keyed file under `.ci-artifacts/cache/`. The report separates kache and sccache, records
the checkout revision and available step/cache costs, and carries bounded
key-history digests for that job. Missing earlier history, provider queue
timestamps or a binary linker observation remain unknown. The report contains
neither raw configuration/environment data nor compiler arguments. GitHub and
GitLab retain it as a job artifact even when the lane fails. Provider upload
paths select only the current job key, so inherited upstream reports are never
re-uploaded as a consumer result if bootstrap fails.

Linux CI containers leave compile admission to their CPU and memory limits.
The root `justfile` disables kache's store-wide scheduler for configured Linux
CI runners before bootstrapping xtask. Kache sizes that shared pool from the
calling container's CPU count, so independent containers would otherwise
compete for one container's slots. Other invocations retain their scheduler
setting, enabled by default.

Each office has RustFS; their firewalls prevent a shared
endpoint. No public S3 route is needed. Mac runs native RustFS under launchd
on `127.0.0.1:9000`. Linux
retains the Compose service published on `127.0.0.1:19000`.

As the Mac CI user, run `ci host mac install-cache` with the profile and pins. It verifies the pinned archive, installs under `toolchains/rustfs/`
and stages `com.zvuk.kithara-ci.cache.plist` without starting it. Scopes and
quotas come from `services/ci-cache/macos.env`. The agent runs
`ci cache serve --root <host-root>/services/ci-cache` with a private
`admin-home` for rc; launchd restarts the supervisor when a child fails.

Drain writers and retain an offline copy before replacing the old container.
Never open the same writable data directory from both servers. Stop the old
container, disable its restart, load the native agent, verify scoped writes
and independent compiler reuse, update runner environments, then resume jobs.
Rollback stops native RustFS before restoring the offline data, configuration
and old container before the guest is retired. After native audit and cache
acceptance, remove the obsolete Colima agent and profile with no jobs running.
Its old RustFS bind mount names the native data directory: delete the VM through
Colima, never the host-mounted service data.

Both supervisors preserve administrator and client identities, initialize
scope buckets and lifecycle rules through `ci::cache::provision`, and run the
audit receiver and evictor beside RustFS. State lives in host directories;
Linux state must survive container resets; do not use Docker named volumes.

Linux uses `docker/ci-cache.compose.yml` and a host-owned environment file.
Docker init propagates stop signals.
Refresh `/etc/kithara-ci/cache-compose/source`, retain the previous tree as
`source.before-<stamp>`, and rebuild to deploy changed bucket policy.

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

Policy checks and routine counters read the pinned kache daemon's IPC
statistics on Unix. They do not run `kache stats --json`, whose disk accounting
walks blobs and synchronizes Linux extents. Full event reports remain available.
Maintenance runs `kache gc` and checks its exit status; its JSON rendering also
probes extents and must not run concurrently with normal CI work.

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

The host profile's `sccache_size` also sets kache's local store target, including
the runner host layer and each job's scoped store. The S3-backed fallback does
not reserve its local limit in advance. Keep this one policy consistent before
introducing a second budget. Protected/import-pinned entries can leave actual
usage above the target; lowering a target does not make those bytes reclaimable.
Size it against the protected working set, persistent Cargo slots, RustFS and
guest disks, retaining the existing physical free-space floor. Unknown protected
byte counts must stay unknown rather than being inferred from entry counts.

Keep two admitted jobs until matched trials qualify a larger setting. For ten
cores the existing worker budget gives four Cargo workers per job at two slots
and three at three slots, including bootstrap. Compare the same revision cohort
and cache-state class in at least three paired runs, retaining failures, swap,
storage pressure and measured-suite timing. More slots are accepted only when
cohort completion improves without violating those contracts; inconclusive
results keep the previous profile.

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

`build_cache_size` caps the build directories visible to one kernel together.
Every CI lane enforces it before admission, even when free space is above the
rejection floor. Mac cleanup owns native builds. Bootstrap xtask
also collects superseded Cargo units under its build and Cargo profile locks,
retaining the newest units, their reachable dependencies and a one-day window.
RustFS holds reusable compiler entries; Cargo still materializes the binaries,
linker inputs and build-script output a job executes in its local target.
Warm builds are the last
rung: when stale trees and compiler-cache collection still leave
the volume at `Aggressive` or worse, the pass reclaims what it is short of the
soft floor, evicting past the cap. What its sweeps cannot show:

- A cache namespace that stops being written to goes invisible rather than
  stale, leaving a retired tool's store behind; `cache_namespaces` lists the
  live ones and cleanup takes the rest whole. A namespace with an owner belongs
  there even when it is quiet: `cargo` holds downloaded packages and `kache` holds
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

Automatic cleanup preserves service data and configuration. If safe cache
reclamation cannot restore the free-space
floor, cleanup reports failure and preflight refuses new jobs. Runtime deletion
is an explicit maintenance operation, never a disk-pressure cleanup step.

Each pass also runs kache's requested collector against native-platform stores,
using the host's size limit and an isolated local-only maintenance runtime.
Automatic kache collection protects remote imports for six hours even above
that limit; host maintenance must enforce the budget without that import pin.
The collector retains its locks and protection for restores in flight. RustFS
bucket eviction bounds remote objects separately and cannot reclaim local copies.
Remote prediction hints expire after one day through bucket lifecycle; they are
re-derived when absent. Compiler manifests and packs remain under the evictor's
last-use policy, so reading an old result does not make it expire by write age.

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
