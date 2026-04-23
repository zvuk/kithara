# Docker CI — full cycle runs

Self-contained Docker image that runs the full CI gate (`lint-fast` → `test` →
`test-e2e` → workspace `cargo mutants`) on one host. Intended for long remote
runs — the `cargo mutants --workspace` stage alone takes hours on ~20 crates.

Canonical invariants live in `AGENTS.md` and crate `README.md` files; this doc
only describes how to drive the container.

## Files

- `docker/ci.Dockerfile` — base image with Rust, cargo tools, FFmpeg dev libs.
- `docker/docker-compose.yml` — `ci-full` service that bind-mounts the repo and
  runs `just ci-full-run /ci-out`.
- `justfile` → `mutants-ci OUTPUT` and `ci-full-run OUTPUT`.

## Local run

```bash
cd /path/to/kithara
docker compose -f docker/docker-compose.yml build           # first build: ~15–30 min
docker compose -f docker/docker-compose.yml up -d
docker compose -f docker/docker-compose.yml logs -f ci-full # follow progress
docker compose -f docker/docker-compose.yml down            # stop + remove container
```

Results land in the host directory `/var/kithara-ci-results/` (create it first:
`sudo mkdir -p /var/kithara-ci-results && sudo chown $USER /var/kithara-ci-results`).
Build artifacts persist in the named volume `kithara_target` across runs.

Smoke test without launching the full pipeline:

```bash
docker compose -f docker/docker-compose.yml run --rm ci-full just lint-fast
docker compose -f docker/docker-compose.yml run --rm ci-full cargo mutants --version
docker compose -f docker/docker-compose.yml run --rm ci-full similarity-rs --version
```

## Reading results

Every stage writes two files under `OUTPUT` (= `/ci-out` inside the container,
`/var/kithara-ci-results` on the host):

- `run-id` — UTC timestamp of the run start.
- `stage-<name>.log` — full stdout+stderr of that stage.
- `stage-<name>.exit` — numeric exit code (one file per stage).
- `mutants/` — `cargo mutants --output` tree for the workspace stage,
  including `mutants.out/outcomes.json` and per-mutant diff logs.

Stages (in order): `lint`, `test`, `e2e`, `mutants`. The pipeline never stops
early — a red stage still produces its `.log` + `.exit`, and the next stage
runs regardless. Check `stage-*.exit` after the run to see which stages
succeeded.

## Server run (startgenai.ru)

One-time setup:

```bash
ssh root@startgenai.ru '
  apt-get update && apt-get install -y docker.io docker-compose-plugin git
  mkdir -p /var/kithara-ci-results
  cd /opt && git clone <remote-url> kithara
'
```

Start the full cycle:

```bash
ssh root@startgenai.ru '
  cd /opt/kithara
  git fetch --all && git checkout main && git pull --ff-only
  docker compose -f docker/docker-compose.yml build
  docker compose -f docker/docker-compose.yml up -d
  docker ps --filter name=ci-full --format "{{.ID}}" \
    > /var/kithara-ci-results/container.id
'
```

Follow progress:

```bash
ssh root@startgenai.ru 'tail -f /var/kithara-ci-results/stage-mutants.log'
ssh root@startgenai.ru 'ls -la /var/kithara-ci-results/ && cat /var/kithara-ci-results/stage-*.exit'
```

Stop and clean up when done:

```bash
ssh root@startgenai.ru 'cd /opt/kithara && docker compose -f docker/docker-compose.yml down'
```

## Tuning

- `mutants-ci` uses `-j $(nproc) - 1`. Override by running `cargo mutants`
  directly with a custom `-j` if needed.
- `--timeout 900` + `--minimum-test-timeout 300` accommodate slow integration
  tests; raise further if mutants report timeouts on green baselines.
- The `kithara_target` named volume caches build artifacts between runs;
  `docker volume rm docker_kithara_target` to start fresh.
