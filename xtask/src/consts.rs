use std::time::Duration;

pub(crate) const INPUT_BYTES: usize = 32 * 1024 * 1024;

pub(crate) const PATCH_BYTES: usize = 16 * 1024 * 1024;

pub(crate) const PATCH_OPERATIONS: usize = 4096;

pub(crate) const PATH_BYTES: usize = 4096;

#[cfg(test)]
pub(crate) const OVERRIDE_ENV: &str = "KITHARA_AGENT_ALLOW_DESTRUCTIVE_GIT";

/// Rust targets the device build compiles, each with its Android ABI name.
pub(crate) const RUST_TARGETS: &[(&str, &str)] = &[
    ("aarch64-linux-android", "arm64-v8a"),
    ("x86_64-linux-android", "x86_64"),
];

pub(crate) const ATTACH_POLL: Duration = Duration::from_millis(500);

pub(crate) const ATTACH_DEADLINE: Duration = Duration::from_secs(180);

pub(crate) const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) const EMULATOR: &str = "the emulator this run booted";

pub(crate) const ORIGIN_PROBE: Duration = Duration::from_secs(5);

pub(crate) const PACKAGE: &str = "com.kithara.nativetest";

#[cfg(test)]
pub(crate) const ASSETS_LIB: &str = r#""kithara-assets": {
        "binary-id": "kithara-assets",
        "binary-name": "kithara_assets",
        "kind": "lib",
        "binary-path": "/deps/kithara_assets-1",
        "build-platform": "target"
    }"#;

#[cfg(test)]
pub(crate) const ASSETS_TEST: &str = r#""kithara-assets::crash_recovery": {
        "binary-id": "kithara-assets::crash_recovery",
        "binary-name": "crash_recovery",
        "kind": "test",
        "binary-path": "/deps/crash_recovery-2",
        "build-platform": "target"
    }"#;

#[cfg(test)]
pub(crate) const MACROS_PROC: &str = r#""kithara-test-macros": {
        "binary-id": "kithara-test-macros",
        "binary-name": "kithara_test_macros",
        "kind": "proc-macro",
        "binary-path": "/deps/kithara_test_macros-3",
        "build-platform": "host"
    }"#;

#[cfg(test)]
pub(crate) const DEVTOOLS_LIB: &str = r#""kithara-devtools": {
        "binary-id": "kithara-devtools",
        "binary-name": "kithara_devtools",
        "kind": "lib",
        "binary-path": "/deps/kithara_devtools-4",
        "build-platform": "host"
    }"#;

#[cfg(test)]
pub(crate) const INTEGRATION_LIB: &str = r#""kithara-integration-tests": {
        "binary-id": "kithara-integration-tests",
        "binary-name": "kithara_integration_tests",
        "kind": "lib",
        "binary-path": "/deps/kithara_integration_tests-5",
        "build-platform": "target"
    }"#;

/// The entry point the instrumentation calls to hand the host transport to Rust.
pub(crate) const TRANSPORT_INSTALL: &str = "Java_com_kithara_net_NativeHttpTransport_install";

#[cfg(test)]
pub(crate) const RENDER: &str = "com.kithara.OfflineCaptureTest#rendersCleanWav";

/// `panic=immediate-abort` lowers every panic to a trap, so the
/// `core::fmt` panic plumbing drops out of each slice; same lane as the
/// wasm flags in `crates/kithara-ffi/.cargo/config.toml`.
///
/// No `embed-bitcode=no` here: `relink_slices_with_lto` runs fat LTO over
/// the slice, and LTO consumes exactly the rlib bitcode that flag
/// suppresses. rustc rejects the two together for the same reason.
pub(crate) const RELEASE_RUSTFLAGS: &[&str] =
    &["-Z", "unstable-options", "-C", "panic=immediate-abort"];

/// Target triples behind each `*.xcframework` slice directory. Universal
/// slices list every arch and are recombined with `lipo -create`.
pub(crate) const SLICE_TARGETS: &[(&str, &[&str])] = &[
    ("ios-arm64", &["aarch64-apple-ios"]),
    (IOS_SIMULATOR_SLICE, &["aarch64-apple-ios-sim"]),
    (
        IOS_SIMULATOR_FAT_SLICE,
        &["aarch64-apple-ios-sim", "x86_64-apple-ios"],
    ),
    (
        "macos-arm64_x86_64",
        &["aarch64-apple-darwin", "x86_64-apple-darwin"],
    ),
];

/// `+nightly` propagates to the nested `cargo build` processes that
/// cargo-swift spawns (rustup exports `RUSTUP_TOOLCHAIN`), which is what
/// activates the `[unstable] build-std` section of
/// `crates/kithara-ffi/.cargo/config.toml` for every slice. `-Z` CLI
/// flags must not be added here: the outer cargo consumes them without
/// forwarding to external subcommands, so they silently do nothing.
pub(crate) const RELEASE_CARGO_ARGS: &[&str] = &["+nightly"];

/// Slice subdirectories inside the `*.xcframework` we expect to find.
pub(crate) const XCFRAMEWORK_SLICES: &[&str] =
    &["ios-arm64", IOS_SIMULATOR_SLICE, "macos-arm64_x86_64"];

pub(crate) const IOS_SIMULATOR_FAT_SLICE: &str = "ios-arm64_x86_64-simulator";

pub(crate) const IOS_SIMULATOR_SLICE: &str = "ios-arm64-simulator";

pub(crate) const CHILD_POLL: Duration = Duration::from_millis(20);

/// How long a child gets to leave after it is killed outright.
pub(crate) const GRACE: Duration = Duration::from_secs(10);

pub(crate) const STATUS_CONTEXT: &str = "kithara/gitlab-verification";

pub(crate) const EPOCH: &str = "1970-01-01T00:00:00Z";

/// Paths that define the trusted `GitLab` judge. Pull-request product content is
/// tested with these paths restored from the synchronized default branch.
///
/// Restoring them keeps a pull request from grading itself, and nothing else:
/// keeping a branch in step with the host is the merge's job, not this one.
/// That separation was learned the hard way — while the two were the same
/// mechanism, exempting trusted authors from the first also took the second
/// away, and their older branches died on the host profile before a test ran.
/// A trusted author's pull request keeps its own control paths on top of the
/// merged base, because a change to the judge has nowhere else to be tested.
///
/// Being listed here is not by itself a reason to reject a pull request. What a
/// change does to the judge decides that, and `control::classify` is where it is
/// decided: an entry added beside the existing ones promises nothing new about
/// the old ones, while an edited or deleted entry can turn a run green without
/// the code earning it.
pub(crate) const CONTROL_PATHS: &[&str] = &[
    ".gitlab-ci.yml",
    ".gitlab/",
    ".config/ci-pins.toml",
    ".config/just/",
    ".config/mutation-suites.toml",
    ".config/nextest.toml",
    ".config/xtask.toml",
    "ci/",
    "docker/",
    "justfile",
    "xtask/",
];

#[cfg(test)]
pub(crate) const CURRENT_BASE: &str = "04fcb5a0a1978c3d1f0e2b3a4c5d6e7f80910111";

#[cfg(test)]
pub(crate) const OLDER_BASE: &str = "418167884f6c2e1d3a4b5c6d7e8f90a1b2c3d4e5";

#[cfg(test)]
pub(crate) const PULL_HEAD: &str = "8a4e697a770d5e6f8091a2b3c4d5e6f708192a3b";

#[cfg(test)]
pub(crate) const RETIRED_HEAD: &str = "6cd1433327cd8f9e0a1b2c3d4e5f60718293a4b5";

pub(crate) const TARGET_SLOT_CACHE_NAMESPACE: &str = "target-slots";

pub(crate) const TARGET_HEARTBEAT_FILE: &str = ".kithara-job-heartbeat";

// Two cleanup intervals tolerate a paused VM while bounding a killed job's
// stale claim. A live helper refreshes this every 30 seconds.
pub(crate) const HEARTBEAT_MAX_AGE: Duration = Duration::from_secs(10 * 60);

pub(crate) const CLIENT_KEYS: [&str; 8] = [
    "SCCACHE_BUCKET",
    "SCCACHE_S3_KEY_PREFIX",
    "SCCACHE_ENDPOINT",
    "SCCACHE_REGION",
    "SCCACHE_S3_USE_SSL",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_EC2_METADATA_DISABLED",
];

/// Where sccache keeps its objects inside a scope's bucket.
///
/// They used to sit at the bucket root with no common prefix, which is why
/// retention had to be a single unfiltered rule expiring everything after a
/// day. That rule also governed the snapshot layers, so a multi-gigabyte
/// source layer would have been republished daily. Naming the compiler cache
/// makes retention expressible per layer. The cost is paid once: existing
/// compiler-cache objects sit at the old keys and are not read again.
pub(crate) const SCCACHE_PREFIX: &str = "sccache";

/// Directory every Unix executor reads the installed host profile from.
pub(crate) const LANE_CONFIG_DIR: &str = "/etc/kithara-ci";

/// Installed profile of the Mac mini and the guests it hosts, read through
/// `KITHARA_CI_HOST_CONFIG`. A Linux machine carries its own; see
/// [`crate::ci::host::linux`].
pub(crate) const MAC_CONFIG_PATH: &str = "/etc/kithara-ci/mac-host.toml";

/// Repository-relative location of the reviewed build pins.
pub(crate) const PINS_PATH: &str = ".config/ci-pins.toml";

/// Keys nextest reads on a profile. Inside a `junit` table it drops them
/// with a warning, which is how `[profile.ci.junit]` swallowed two of them.
#[cfg(test)]
pub(crate) const PROFILE_KEYS: &[&str] =
    &["fail-fast", "leak-timeout", "slow-timeout", "test-threads"];

/// The integration suite opens far more files across its cache and segment
/// fixtures than the 256 descriptor soft limit a macOS session starts
/// with. The lane raises its own ceiling so every executor gets the same
/// budget.
#[cfg(unix)]
pub(crate) const OPEN_FILES: u64 = 65536;

/// How often the gate re-asks a volume it is waiting on. What it waits for
/// is a neighbouring job ending, so it re-asks on the scale a job finishes
/// on rather than the scale a compiler writes files on.
pub(crate) const JOB_ROOM_POLL: Duration = Duration::from_secs(15);

/// Where a runner mounts the build root every lane claims a directory under.
/// Absent on an executor that builds in its checkout.
pub(crate) const TARGET_ROOT_ENV: &str = "KITHARA_CI_TARGET_ROOT";

#[cfg(test)]
#[cfg(unix)]
pub(crate) const CACHE_ROOT: &str = "KITHARA_TEST_CACHE_ROOT";

#[cfg(test)]
#[cfg(unix)]
pub(crate) const FAILED_PREPARE: &str = "KITHARA_TEST_FAILED_ENV_CHILD";

#[cfg(test)]
#[cfg(unix)]
pub(crate) const LANE_PREPARED: &str = "KITHARA_TEST_LANE_PREPARED_ENV_CHILD";

#[cfg(test)]
#[cfg(unix)]
pub(crate) const PREPARED: &str = "KITHARA_TEST_PREPARED_ENV_CHILD";

/// Build cache older than this is rebuilt faster than it is worth keeping.
pub(crate) const BUILD_CACHE_AGE: &str = "168h";

#[cfg(test)]
pub(crate) const LISTED: &str = "kithara-ci:linux-20260729\n\
                      kithara-ci:linux-20260806d\n\
                      kithara-ci-android:linux-20260806c\n\
                      kithara-ci-android-runner:linux-20260806c\n\
                      kithara-ci-runner:linux-20260806d\n";

/// Where the generated project is written when no path is given.
pub(crate) const FILE: &str = "/etc/kithara-ci/docker-compose.yml";

/// Where a job builds and what it reuses, before the linker entries are added.
pub(crate) const CACHE_ENVIRONMENT: [&str; 7] = [
    // Encoded audio fixtures. Their default home is the container's own temp
    // directory, and a container serves one job and is thrown away — so every
    // job re-encoded every fixture it touched, and a test that builds one
    // inside its own deadline lost the race under load. Entries are
    // content-addressed and namespaced by a build fingerprint, so sharing them
    // across runners cannot serve one build's bytes to another.
    "KITHARA_FIXTURE_CACHE=/cache/fixtures",
    // Every runner mounts this one root, and a lane claims the directory named
    // after it underneath: a lane that lands on a different runner than last
    // time then still finds its own warm build. A runner-owned directory made
    // that a full rebuild, which is most of what a lane spent its time on.
    "KITHARA_CI_TARGET_ROOT=/cache/lanes",
    // What a job builds into when it claims no lane directory. It stays one
    // directory per runner, on a path of its own, so a checkout that predates
    // the lane keying keeps exactly the cache it reused before instead of
    // meeting every other job in one cargo lock.
    "CARGO_TARGET_DIR=/cache/target",
    "RUSTC_WRAPPER=sccache",
    // Without this the wrapper is inert: sccache declines to cache an
    // incremental compilation, and cargo leaves incremental on by default.
    // Setting the wrapper and not this is how a cache gets installed, enabled,
    // and still never hit.
    "CARGO_INCREMENTAL=0",
    // GitHub checks each job out under this stable container path. Without a
    // base directory sccache hashes the host-specific checkout path, so two
    // otherwise identical runners cannot reuse Rust objects.
    "SCCACHE_BASEDIRS=/runner/_work/kithara/kithara",
    // Well under the volume it lives on, and sccache evicts by least use
    // rather than growing until the disk decides for it.
    "SCCACHE_CACHE_SIZE=100G",
];

/// Address blocks a job has no business reaching. The machine's neighbours live
/// on private addresses, and a CI job that can open a port on them is a CI job
/// that can read a database it was never given.
pub(crate) const PRIVATE_BLOCKS: [&str; 3] = ["172.16.0.0/12", "10.0.0.0/8", "192.168.0.0/16"];

/// A mode only its owner may read and write.
pub(crate) const OWNER_ONLY: u32 = 0o600;

/// A mode anyone may read and run, and only its owner may write.
pub(crate) const EXECUTABLE: u32 = 0o755;

/// Installed profile every Linux CI machine reads through
/// `KITHARA_CI_LINUX_CONFIG`.
pub(crate) const LINUX_CONFIG_PATH: &str = "/etc/kithara-ci/linux-host.toml";

pub(crate) const API_VERSION: &str = "X-GitHub-Api-Version";

/// The uid the runner image runs its jobs as. It is the image's, not this
/// machine's, so it is written beside the code that mounts into that image.
pub(crate) const JOB_USER: u32 = 1000;

pub(crate) const HOST_PACKAGES: [&str; 10] = [
    "iptables",
    "dnsmasq-base",
    "qemu-utils",
    "nvidia-container-toolkit",
    "qemu-system-x86",
    "libvirt-daemon-system",
    "ovmf",
    "swtpm-tools",
    "virtinst",
    "xorriso",
];

/// Where the guest reaches the shared directories.
///
/// virtiofs auto-mounts them under `/Volumes/My Shared Files`, and GNU make
/// cannot represent a path containing spaces at all — space is its separator
/// between targets, with no escape. `xcrun` resolves symlinks before handing
/// the toolchain path to cmake, cmake writes it into the generated Makefile,
/// and make then reports `/Volumes/My: No such file or directory`. Only a real
/// mount elsewhere fixes it, so the guest moves the share here on startup.
pub(crate) const GUEST_SHARE: &str = "/opt/kithara";

/// The floating tag the Linux runner runs. It always names the image the most
/// recent pipeline pinned, so the runner configuration never changes with a pin.
pub(crate) const LINUX_LATEST_IMAGE: &str = "kithara-ci:linux-latest";

/// The throwaway VM the macOS lane clones for every job.
pub(crate) const JOB_VM_NAME: &str = "kithara-ci-job";

pub(crate) const BOOT_ATTEMPTS: u32 = 40;

pub(crate) const BOOT_POLL: Duration = Duration::from_secs(5);

pub(crate) const WAIT_SECONDS: u32 = 7200;

/// How many jobs a guest serves before it is thrown away. The build
/// directory is kept between jobs, so it only ever grows, and the guest's
/// own disk is what runs out first.
///
/// Measured over one nightly: the 90-gigabyte disk presents a 78-gibibyte
/// container, macOS and its swap hold about 20 of it, and eleven macOS
/// jobs left 44 gibibytes under `target` — 18 in `debug`, 9 in
/// `test-release`, the rest spread over one directory per Apple triple.
/// That reached 114 mebibytes free, and `apple:ios` stopped in `lipo` on
/// "No space left on device" while writing the universal archive.
///
/// Six jobs keeps the peak near 30 gibibytes, which leaves room for the
/// transient a universal link needs. It costs one extra cold build per
/// nightly. Raising it again means giving the guest a larger disk, and
/// that needs room on the CI volume the quota does not currently allow.
pub(crate) const MAX_BUILDS: u32 = 6;

pub(crate) const LAUNCHCTL: &str = "/bin/launchctl";

pub(crate) const AGENT_RUNNING: &str = "running";

/// A host with no launchd has no agents to be wrong about, and the Linux
/// executor runs this same command.
pub(crate) const AGENT_ABSENT: &str = "not-applicable";

#[cfg(test)]
pub(crate) const FREE_NORMAL: u64 = 100;

#[cfg(test)]
pub(crate) const FREE_AGGRESSIVE: u64 = 20;

/// Verbatim from the host while the macOS runner was crash-looping.
#[cfg(test)]
pub(crate) const CRASH_LOOP_LISTING: &str = "\
82778\t0\tcom.zvuk.kithara-ci.gitlab-runner
-\t0\tcom.zvuk.kithara-ci.health
-\t1\tcom.zvuk.kithara-ci.macos-runner
-\t0\tcom.zvuk.kithara-ci.cleanup
54543\t0\tcom.zvuk.kithara-ci.colima
";

/// Shape produced by `diskutil apfs list` on the CI host: every field line
/// carries the container's `|` tree guides.
#[cfg(test)]
pub(crate) const APFS_LIST: &str = "\
|   +-> Volume disk3s6 4E1F0D1A-0000-0000-0000-000000000000\n\
|   |   ---------------------------------------------------\n\
|   |   APFS Volume Disk (Role):   disk3s6 (VM)\n\
|   |   Name:                      VM (Case-insensitive)\n\
|   |   Capacity Consumed:         2147504128 B (2.1 GB)\n\
|   |\n\
|   +-> Volume disk3s7 07077B7C-0000-0000-0000-000000000000\n\
|       ---------------------------------------------------\n\
|       APFS Volume Disk (Role):   disk3s7 (No specific role)\n\
|       Name:                      KitharaCI (Case-sensitive)\n\
|       Mount Point:               /Volumes/KitharaCI\n\
|       Capacity Consumed:         28917514240 B (28.9 GB)\n\
|       Capacity Reserve:          None\n\
|       Capacity Quota:            300000002048 B (300.0 GB) (9.6% reached)\n\
|       Sealed:                    No\n";

/// The simulator shares the host network stack, so it reaches the server
/// over loopback. The port is fixed because Apple simulator suites are
/// serialized on the host, so another CI lane cannot bind it concurrently.
pub(crate) const TEST_SERVER_PORT: u16 = 3444;

/// Held by the one job building in the directory.
pub(crate) const LOCK_FILE: &str = ".kithara-lane.lock";

/// The content the directory's artifacts may have been built from.
pub(crate) const SOURCES_FILE: &str = ".kithara-lane-sources";

/// Stands for content a build the record did not see may have used.
pub(crate) const UNKNOWN_BLOB: &str = "unknown";

#[cfg(test)]
pub(crate) const FIXTURE_FAILURE_EXIT_CODE: i32 = 7;

pub(crate) const CONNECTION_REFUSED_CODE: Option<&str> = if cfg!(target_os = "macos") {
    Some("(os error 61)")
} else if cfg!(target_os = "linux") {
    Some("(os error 111)")
} else if cfg!(windows) {
    Some("(os error 10061)")
} else {
    None
};

pub(crate) const SCCACHE_COMMAND_ERROR: i32 = 2;

pub(crate) const SCCACHE_CONNECT_ERROR: &str = "sccache: error: couldn't connect to server";

pub(crate) const SCCACHE_MISSING_UDS_CODE: Option<&str> = if cfg!(unix) {
    Some("(os error 2)")
} else {
    None
};

pub(crate) const SCCACHE_STOP_MESSAGE: &str = "Stopping sccache server...";

/// Lanes no pipeline schedules and no fleet claims. Each one is reached by
/// name alone and says so with empty membership, which is a declaration
/// rather than an oversight - and naming them here is what keeps a lane
/// that merely forgot its membership from hiding among them.
#[cfg(test)]
pub(crate) const BY_NAME_ONLY: [&str; 1] = ["deep-ui"];

/// Current default linker for Linux CI jobs, as target-scoped Cargo variables.
///
/// Cargo timing establishes that the build dominates the test lane but does
/// not split code generation from linking, so `lld` is a controlled candidate,
/// not a root-cause conclusion. The image also carries `mold` for a separately
/// measured target-scoped override. `sccache` cannot reuse final link outputs.
///
/// Scoped per target rather than through `RUSTFLAGS`, which would follow the
/// wasm and Apple builds to hosts that have no `ld.lld`. Both Linux triples are
/// named because the fleet is x86-64 and the image builds on Apple silicon;
/// the one that does not apply is inert.
pub(crate) const LINUX_LINKER_ENV: [(&str, &str); 2] = [
    (
        "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
        LINUX_LINKER_RUSTFLAGS,
    ),
    (
        "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
        LINUX_LINKER_RUSTFLAGS,
    ),
];

pub(crate) const LINUX_LINKER_RUSTFLAGS: &str = "-Clink-arg=-fuse-ld=lld";

/// Host-global lock namespace that coordinates the compiler-cache slots.
pub(crate) const SCCACHE_SLOT_CONTROL_NAMESPACE: &str = ".kithara-ci-sccache-slots";

/// CI-owned compiler-cache slots, kept disjoint from the local cache directory.
pub(crate) const SCCACHE_SLOT_CACHE_NAMESPACE: &str = "sccache-slots";

/// A runner owns one cache daemon for the life of its job or container.
///
/// The daemon can be ready well before a lane reaches its first compiler
/// process, so the default idle expiry would make an otherwise initialized
/// cache disappear and force concurrent clients to race its restart.
pub(crate) const SCCACHE_IDLE_TIMEOUT: &str = "0";

/// How many `main` runs the journal keeps. One is not enough: a test that fails
/// a quarter of the time would otherwise land in a branch's column whenever the
/// single remembered run happened to be green, and block on its own noise.
pub(crate) const REMEMBERED_RUNS: usize = 5;

/// Where the verdict expects every lane to leave what it produced. Artifacts
/// travel between runners at their own paths, so one directory is what makes a
/// report from the simulator and a report from a container comparable.
pub(crate) const REPORT_DIR: &str = ".ci-artifacts/junit";

/// The pipeline kinds a lane may name. Kept beside the cache groups, which the
/// catalog already validates by name for the same reason: the executor's enum
/// lives in `ci::run`, and a lane is refused at configuration load, before any
/// executor is consulted. `lane_config.rs` pins the two lists together.
pub(crate) const PIPELINE_KINDS: [&str; 8] = [
    "branch",
    "platforms",
    "merge-request",
    "quarantine",
    "main",
    "nightly",
    "weekly",
    "release",
];

pub(crate) const LANE_ROLES: [&str; 6] =
    ["gate", "platforms", "deep", "mutants", "quality", "release"];

/// The lane's own executable. A Windows job runs the binary it started as
/// rather than `cargo xtask`, which would rebuild it - and Windows refuses to
/// replace a running image, so Cargo reported that as a failure to remove
/// `xtask.exe`.
pub(crate) const SELF_PROGRAM: &str = "<xtask>";

/// The checkout a lane resolves in. A compiler flag that has to name a file in
/// the repository needs an absolute path, and only the runner knows it.
pub(crate) const ROOT_PLACEHOLDER: &str = "{root}";

/// The build-cache directory the process leased, i.e. `CARGO_TARGET_DIR` as
/// the executor set it, not `{root}/target`. A lane that writes its own build
/// under a fixed `{root}`-relative path escapes the lease the eviction and
/// reclaim machinery tracks; one that asks for `{target}` stays inside it the
/// same way the process's own build does.
pub(crate) const TARGET_PLACEHOLDER: &str = "{target}";

/// A reviewed pin, by name: `{pin.msrv_toolchain}` is the value that key holds
/// in `.config/ci-pins.toml`.
pub(crate) const PIN_PREFIX: &str = "{pin.";

pub(crate) const CONFIG_PATH: &str = ".config/mutation-suites.toml";

/// The one control this programme photographs by itself, so the shortest
/// capture path runs somewhere. Any page that draws a control by a known
/// path would do; this one is pinned so a page that stops drawing it says
/// so.
pub(crate) const ELEMENT_PAGE: &str = "clock";

pub(crate) const ELEMENT_PATH: &str = "clock-components/title";

/// What a page of the gallery, and a shipped studio page, are allowed to
/// differ by before the programme ends non-zero.
pub(crate) const GALLERY_BUDGET: &str = "crates/kithara-ui/examples/gallery/parity-budget.txt";

/// The sets this programme writes, cleared before it starts so a set left
/// by an earlier run cannot be compared as if this run had taken it.
pub(crate) const SETS: [&str; 5] = ["iced", "masonry", "masks", "parts", "studio"];

pub(crate) const STUDIO_BUDGET: &str = "crates/kithara-app/assets/ui/parity-budget.txt";

/// Where the studio capture is told to write its two sets. It is driven
/// from a test, and a test has no command line of its own to be told on.
pub(crate) const STUDIO_CAPTURE: &str = "KITHARA_STUDIO_CAPTURE";

/// User-agent used for registry availability checks when the project
/// config leaves `publish.user_agent` empty.
pub(crate) const DEFAULT_USER_AGENT: &str = "xtask-publish";

/// Who every commit the release makes is recorded as. The commits land in a
/// public repository, so they carry no person's name or address.
pub(crate) const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "kithara-release"),
    ("GIT_AUTHOR_EMAIL", "kithara-release@localhost"),
    ("GIT_COMMITTER_NAME", "kithara-release"),
    ("GIT_COMMITTER_EMAIL", "kithara-release@localhost"),
];

#[cfg(test)]
pub(crate) const CHANGELOG: &str = "# Changelog\n\n\
    ## [0.0.2](https://github.com/zvuk/kithara/releases/tag/v0.0.2) - 2026-09-24\n\n\
    ### Features\n\n- **audio**: Faster seeks\n\n\
    ## [0.0.1](https://github.com/zvuk/kithara/releases/tag/v0.0.1) - 2026-07-01\n\n\
    ### Fixed\n\n- Older fix\n";

/// The section's own layout; the panel, tabs, and palette are the player
/// page's, so the section reads as part of the page it is added to.
pub(crate) const STYLE: &str = "
.release{flex:1 1 460px;min-width:0}
.release a{color:var(--accent-strong);text-decoration:none}
.release a:hover{text-decoration:underline}
.release-version{color:var(--accent-strong);font:600 12px var(--font-mono)}
.release-nav{display:flex;gap:14px;margin-left:auto;font-size:12px}
.release-count{margin-left:6px;padding:0 5px;background:color-mix(in srgb,var(--accent) 18%,transparent);color:var(--accent-strong)}
.release-docs{display:grid;grid-template-columns:repeat(auto-fit,minmax(100px,1fr));gap:8px}
.release-docs a{display:flex;flex-direction:column;gap:2px;padding:10px 12px;border:1px solid var(--line);background:var(--bg-dark);color:var(--text-main);font-weight:600}
.release-docs a:hover{border-color:var(--accent);color:var(--accent-strong);text-decoration:none}
.release-docs span{color:var(--text-muted);font-size:11px;font-weight:400}
.release-files,.release-crates{list-style:none}
.release-files li{display:grid;grid-template-columns:minmax(0,1fr) auto;align-items:center;gap:2px 16px;padding:8px 0}
.release-files li+li{border-top:1px solid var(--line)}
.release-files a{overflow-wrap:anywhere;font:12px var(--font-mono)}
.release-files code{grid-row:span 2;max-width:12ch;overflow:hidden;color:var(--text-muted);font:11px var(--font-mono);text-overflow:ellipsis;white-space:nowrap;user-select:all}
.release-files span{color:var(--text-muted);font-size:12px}
.release-crates{display:grid;grid-template-columns:repeat(auto-fill,minmax(240px,1fr));gap:0 16px}
.release-crates li{display:flex;align-items:center;gap:10px;padding:5px 0;border-bottom:1px solid var(--line);font-size:11px}
.release-crates span{flex:1;min-width:0;overflow:hidden;color:var(--text-main);font:12px var(--font-mono);text-overflow:ellipsis;white-space:nowrap}
.release-foot{padding:8px 12px;border-top:1px solid var(--line);color:var(--text-muted);font-size:11px}
";

#[cfg(test)]
pub(crate) const MANIFEST_SAMPLE: &str =
    "// header\nlet version = \"0.0.1-alpha3\"\nlet checksum = \"abc\"\nlet other = 1\n";

pub(crate) const BUILD_OUTPUT_LIMIT: usize = 4096;

#[cfg(unix)]
pub(crate) const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[cfg(unix)]
pub(crate) const CHILD_TERMINATION_GRACE: Duration = Duration::from_secs(2);

pub(crate) const BUFFER_SIZE: usize = 64 * 1024;

pub(crate) const BINARY: &str = if cfg!(windows) { "xtask.exe" } else { "xtask" };

pub(crate) const CACHE_DIRECTORY: &str = "xtask-cache";

pub(crate) const GENERATION_PREFIX: &str = "generation-";

pub(crate) const LEASE_FILE: &str = "lease.lock";

pub(crate) const LOCATOR_FILE: &str = "active";

pub(crate) const MANIFEST_FILE: &str = "manifest.json";

pub(crate) const REFRESH_LOCK: &str = "refresh.lock";

pub(crate) const STAMP_FILE: &str = "stamp";

pub(crate) const CONTROL_FILE_LIMIT: usize = 16 * 1024;

pub(crate) const BUILD_RECIPE: u32 = 1;

pub(crate) const CACHE_SCHEMA: u32 = 1;

pub(crate) const MANIFEST_LIMIT: usize = 1024 * 1024;

pub(crate) const RESERVATION_ATTEMPTS: usize = 1_000;

/// The line the server prints once its listener is bound. Its origin is
/// `STARTUP_RECORD` in `tests/crates/integration/src/test_server/native.rs`.
pub(crate) const STARTUP_RECORD: &str = "test server listening on";

pub(crate) const TEST_SERVER_POLL: Duration = Duration::from_millis(200);

pub(crate) const READY: Duration = Duration::from_secs(60);

pub(crate) const TEXT_DECODER_POLYFILL: &str = "\
if(typeof TextDecoder===\"undefined\"){\
globalThis.TextDecoder=class{constructor(){}\
decode(b){if(!b||!b.length)return\"\";let r=\"\";\
for(let i=0;i<b.length;i++)r+=String.fromCharCode(b[i]);return r}};\
globalThis.TextEncoder=class{constructor(){}\
encode(s){const a=new Uint8Array(s.length);\
for(let i=0;i<s.length;i++)a[i]=s.charCodeAt(i);return a}\
encodeInto(s,d){const e=this.encode(s);d.set(e);\
return{read:s.length,written:e.length}}}}\n";

pub(crate) const BOOT_LOCK_FN: &str = "\
function __wstLockedInit(s,mod,mem,m){\
const bl=(m&&m.__wst_boot_lock_ptr)||0;\
if(bl>0){\
const li=new Int32Array(mem.buffer),lx=bl>>>2;\
while(Atomics.compareExchange(li,lx,0,1)!==0)Atomics.wait(li,lx,1,10);}\
try{s.initSync({module:mod,memory:mem,thread_stack_size:1048576});}\
finally{if(bl>0){\
const li2=new Int32Array(mem.buffer),lx2=bl>>>2;\
Atomics.store(li2,lx2,0);Atomics.notify(li2,lx2);}}}";

pub(crate) const CHECK_RUNTIME_JS: &str = r#"

// --- Auto-generated by xtask wasm postbuild ---

/**
 * Check if the browser environment supports SharedArrayBuffer + COEP/COOP.
 * Returns { ok: boolean, reason?: string, waitingForReload?: boolean }.
 */
export function checkRuntime() {
    const crossOriginIsolated = self.crossOriginIsolated === true;
    const secureContext = self.isSecureContext === true;
    const sharedArrayBuffer = typeof SharedArrayBuffer !== 'undefined';

    if (secureContext && sharedArrayBuffer && crossOriginIsolated) {
        return { ok: true };
    }

    // coi-serviceworker.js reloads an unisolated page once its worker controls it.
    if (secureContext && !crossOriginIsolated && 'serviceWorker' in navigator) {
        return { ok: false, waitingForReload: true, reason: 'Waiting for COI service worker to activate' };
    }

    return {
        ok: false,
        waitingForReload: false,
        reason: `secureContext=${secureContext} crossOriginIsolated=${crossOriginIsolated} sharedArrayBuffer=${sharedArrayBuffer}`,
    };
}
"#;

/// How long to wait for the guest to start installing, in one-second
/// attempts, answering the boot prompt until it does.
pub(crate) const GUEST_PROMPT_ATTEMPTS: u32 = 40;

/// How long to wait for an enrolled guest to report for work, in ten-second
/// attempts. A guest that has just been given its credentials signs in,
/// enrols, and starts the runner, which takes minutes.
pub(crate) const GUEST_ENROLMENT_ATTEMPTS: u32 = 60;

/// How long to watch for the power-off that ends the first install phase, in
/// thirty-second attempts.
pub(crate) const GUEST_FIRST_PHASE_ATTEMPTS: u32 = 60;

/// A tracked file a Windows guest is built from, rather than anything pasted
/// into a machine by hand.
pub(crate) const GUEST_ANSWER_FILE: &str = ".config/windows/autounattend.xml";

/// The other tracked file a Windows guest is built from.
pub(crate) const GUEST_PROVISION_SCRIPT: &str = ".config/windows/provision.ps1";

/// The one vendor download a guest is built from, which cannot be pinned.
pub(crate) const GUEST_BUILD_TOOLS_URL: &str = "https://aka.ms/vs/17/release/vs_buildtools.exe";

/// The tools the lane needs on a guest beyond the toolchain.
pub(crate) const GUEST_CARGO_TOOLS: [&str; 2] = ["cargo-nextest", "just"];

/// Where the Linux host's services live.
pub(crate) const SERVICE_SYSTEMD_ROOT: &str = "/etc/systemd/system";

/// What the services call. The executable is copied out of the build tree:
/// run from there it expects to find the repository around it, and a service
/// starting from no particular directory would not.
pub(crate) const SERVICE_EXECUTABLE: &str = "/usr/local/bin/kithara-ci";

pub(crate) const SERVICE_CLEANUP_UNIT: &str = "kithara-ci-cleanup.service";

pub(crate) const SERVICE_CLEANUP_TIMER: &str = "kithara-ci-cleanup.timer";
