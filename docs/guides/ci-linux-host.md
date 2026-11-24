# Linux CI host provisioning

Linux `compiler_cache_root` can place the scoped compiler stores on a separate
volume from build outputs; when omitted, they live under `cache_root/kache`.

A Linux runner requires an explicit `[runners.target]` with `provider = "github"`
and `repository`, or `provider = "gitlab"`, `url`, numeric `project` and
`token_file`. There is no default provider. GitHub profiles list each served
repository with its own credential file. A GitHub registration reaches one repository, so
repositories are peers with no default. A runner naming an uncredentialed
repository is refused at profile read; otherwise it registers against whatever
token is present and reports to the wrong repository. Write each token with
`install -m 600 -o root -g root /dev/stdin`: `sudo` overrides an inherited
umask, so `tee` leaves it world-readable.

Existing Linux profiles must move each runner's `repository` into its explicit
GitHub target before installing the matching binary. Do not overwrite a live
profile or executable during this migration. Installation stages the validated
profile, pins and executable together under a content-addressed directory in
`cache_root/services`; future systemd starts name that immutable deployment.
Current jobs keep their executable, configuration and output ownership.

Declare private control/cache destinations as exact TCP socket addresses in
`private_egress`, for example `["10.20.30.40:443", "172.18.0.2:9000"]`.
These are protected deployment bindings, not additional coverage switches.
Only the runner subnet enters its two owned filter chains; unlisted private
networks and other host services remain blocked. Applying replaces those
chains and their scoped jumps in one `iptables-restore --noflush` transaction;
it never flushes unrelated rules. Established connections and declared endpoints return to the enclosing host policy;
foreign denies still apply. Endpoint or network changes require the safe
admission boundary and a new reviewed deployment snapshot. Resolve/verify the
actual addresses before application; no entire private CIDR is opened.

Corporate Linux profiles declare GitLab targets only, omit `repositories`, and
hold no GitHub credentials. `[gitlab]` names the installed native runner binary,
owned systemd service, active configuration file and helper image by immutable
repository digest. The installed runner must match `gitlab_runner_version` in
the reviewed pins. Install that native prerequisite through host setup; a missing
binary or mismatched version blocks configuration rather than downloading a substitute.

Project-scoped registrations are issued through the existing GitLab project
control plane. Keep its API credentials on the operator controller, outside jobs.
Each target's mode-0600 JSON enrollment receipt binds the issued native identity:

```json
{
  "url": "https://gitlab.example",
  "project": 42,
  "id": 101,
  "token": "<issued runner authentication token>",
  "labels": ["kithara-linux"],
  "protected": false
}
```

Record tags and protection from the actual project registration; generating TOML
does not change those server-side properties. Trusted cache workers require a
protected registration. Once installed, the native manager owns token rotation;
reconfiguration retains its current token and expiry metadata. Enrollment receipts
must still identify the same server, project and runner. Never print receipts,
cache environment values or generated native configuration into CI logs.

For first standalone setup, prepare the existing scoped cache service and its
image-build environment, then build the reviewed image outside CI. Ordinary CI
keeps its required-cache gate. Plain Linux checks need neither nested virtualization
nor QEMU/GPU host packages. Optional devices are checked before setup; a missing
device blocks that worker. GPU runtime preparation is an explicit host operation
and this provisioner never restarts Docker to configure it.

```sh
just ci host linux --config /etc/kithara-ci/linux-host.toml install-tools
just ci host linux --config /etc/kithara-ci/linux-host.toml image toolchain
just ci host linux --config /etc/kithara-ci/linux-host.toml bootstrap
just ci host linux --config /etc/kithara-ci/linux-host.toml gitlab-config --out /root/gitlab-candidate.toml
just ci host linux --config /etc/kithara-ci/linux-host.toml install-services
```

Stage the candidate separately from the active file; physical path aliases are rejected.
Cache configuration and the loaded resource slice precede native admission. Native parsing runs without
printing tokens. Applying validates the declared manager binding and rejects
installed workers outside the profile; it retains native global settings and a
protected rollback copy. Existing managers reload configuration without restart;
occupied GitHub ephemeral units also retain their current processes. If the active
file changes during preparation, application stops before replacement.

Both backends reuse Container CPU/memory/nice limits, scoped Cargo/compiler storage,
target slots, leases and room admission. Native nice serialization is
`[runners.docker.ulimit] nice = "40:40"`, never a `nice=40` key. Jobs use UID/GID1000
without a Docker control socket; the operator manager uses the local Docker API.
The job image must declare `USER runner` so standard native checkout/tmp ownership
works. Optional `ca_file` names an existing approved bundle; provisioning changes
neither trust stores nor certificate verification.

Stage/install success is preparation. Require actual pinned-image jobs with
checkout, resource, cache-reuse, report/artifact and concurrent-output witnesses
before production placement. A manager's active service state alone is not that proof.

Common storage, cache service and room-admission policy remain in [Dedicated CI host](ci-host.md).
