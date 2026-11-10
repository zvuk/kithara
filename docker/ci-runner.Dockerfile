# A GitHub Actions runner that carries the CI toolchain in the image.
#
# Jobs therefore install nothing: no toolchain action, no package step, and no
# third-party action beyond the checkout itself. `CI_IMAGE` is the tag built
# from docker/ci.Dockerfile, so a self-hosted job sees the same toolchain as
# every other lane. Build it through `just ci image runner`, which passes every
# argument below from the reviewed pins.
ARG CI_IMAGE
FROM ${CI_IMAGE}

ARG ACTIONS_RUNNER_AMD64_SHA256
ARG ACTIONS_RUNNER_AMD64_URL
ARG ACTIONS_RUNNER_ARM64_SHA256
ARG ACTIONS_RUNNER_ARM64_URL

# The runner is a .NET application and needs the ICU libraries at run time.
#
# A job gets a cargo home of its own rather than writing into the one the
# image was built with: that one belongs to root, and every file cargo wants
# to create or lock inside it — a registry index, a git checkout, the download
# lock — is a separate way for a job to be denied. The toolchain itself is
# reached through `PATH`, which already names the image's `bin`, so only the
# mutable state moves.
#
# The host mounts a cache volume over the cargo home and over the target
# directory. Docker creates a mount point the image does not have, and creates
# it owned by root, so both are declared here: an empty volume mounted over a
# directory that exists inherits that directory's owner.
RUN apt-get update && apt-get install -y --no-install-recommends \
    libicu72 \
 && rm -rf /var/lib/apt/lists/* \
 && install -d -o runner -g runner /runner /cache/target /home/runner/.cargo

ENV CARGO_HOME=/home/runner/.cargo

USER runner
WORKDIR /runner

RUN case "$(dpkg --print-architecture)" in \
      amd64) url="${ACTIONS_RUNNER_AMD64_URL}"; sum="${ACTIONS_RUNNER_AMD64_SHA256}" ;; \
      arm64) url="${ACTIONS_RUNNER_ARM64_URL}"; sum="${ACTIONS_RUNNER_ARM64_SHA256}" ;; \
      *) echo "unsupported architecture: $(dpkg --print-architecture)" >&2; exit 1 ;; \
    esac \
 && curl -fsSL -H 'Accept: application/octet-stream' \
      -o /tmp/runner.tar.gz \
      "${url}" \
 && echo "${sum}  /tmp/runner.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/runner.tar.gz -C /runner \
 && rm /tmp/runner.tar.gz

# The daemon is ready before this one-shot runner registers for a job. A cache
# backend that cannot initialize must recycle the runner before it can accept a
# job, rather than fail after the checkout and bootstrap have begun.
# The just-in-time configuration is then read once and dropped from the
# environment, so a job's own steps never inherit the credentials that
# registered them.
ENTRYPOINT ["/bin/sh", "-c", "config=${ACTIONS_RUNNER_JITCONFIG:?the host must supply a just-in-time runner configuration}; unset ACTIONS_RUNNER_JITCONFIG; sccache --start-server || exit $?; exec ./run.sh --jitconfig \"$config\""]
