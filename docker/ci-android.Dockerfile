# An Android emulator that runs the instrumented tests on this machine.
#
# The Mac mini emulates a foreign architecture and pays for it; here the guest
# is the host's own, and /dev/kvm makes it a virtual machine rather than an
# interpreter. Build it through `just ci image android`, which passes every
# argument below from the reviewed pins.
ARG CI_IMAGE
FROM ${CI_IMAGE}

ARG ANDROID_AVD
ARG ANDROID_BUILD_TOOLS_VERSION
ARG ANDROID_CMDLINE_TOOLS_LINUX_SHA256
ARG ANDROID_CMDLINE_TOOLS_VERSION
ARG ANDROID_NDK_VERSION
ARG ANDROID_PLATFORM_VERSION
ARG CARGO_NDK_VERSION

ENV ANDROID_HOME=/opt/android-sdk
ENV ANDROID_USER_HOME=/opt/android-user
ENV ANDROID_AVD_HOME=/opt/android-user/avd
ENV JAVA_HOME=/usr/lib/jvm/kithara-jdk

# The emulator needs a windowing stack even when it draws nothing, and the
# Gradle side needs a JDK the Android plugin accepts. Debian names the JDK
# after the architecture, so it is published under one name the tools can
# reach on either.
RUN apt-get update && apt-get install -y --no-install-recommends \
    -o Acquire::Retries=5 -o Acquire::http::Timeout=600 \
    openjdk-17-jdk-headless unzip \
    libgl1 libpulse0 libx11-6 libxcb1 libxcursor1 libxdamage1 libxi6 \
    libxrandr2 libxtst6 \
 && rm -rf /var/lib/apt/lists/* \
 && ln -s "/usr/lib/jvm/java-17-openjdk-$(dpkg --print-architecture)" "${JAVA_HOME}"

RUN curl -fsSL -o /tmp/cmdline-tools.zip \
      "https://dl.google.com/android/repository/commandlinetools-linux-${ANDROID_CMDLINE_TOOLS_VERSION}_latest.zip" \
 && echo "${ANDROID_CMDLINE_TOOLS_LINUX_SHA256}  /tmp/cmdline-tools.zip" | sha256sum -c - \
 && mkdir -p "${ANDROID_HOME}/cmdline-tools" \
 && unzip -q /tmp/cmdline-tools.zip -d /tmp/cmdline-tools \
 && mv /tmp/cmdline-tools/cmdline-tools "${ANDROID_HOME}/cmdline-tools/latest" \
 && rm -rf /tmp/cmdline-tools.zip /tmp/cmdline-tools

ENV PATH=${PATH}:${ANDROID_HOME}/cmdline-tools/latest/bin:${ANDROID_HOME}/platform-tools:${ANDROID_HOME}/emulator

# sdkmanager refuses to install anything until the SDK licences are accepted,
# and it reads the answers from standard input. The system image matches the
# machine, which is the whole point of running the emulator here: a guest whose
# architecture is the host's is virtualised rather than interpreted.
#
# Booting an AVD writes back into it, so what the build creates is handed to the
# account the job runs as rather than kept by the one that created it.
RUN export JAVA_TOOL_OPTIONS="-Dsun.net.client.defaultConnectTimeout=30000 -Dsun.net.client.defaultReadTimeout=120000" \
 && image="system-images;android-${ANDROID_PLATFORM_VERSION};google_apis;$(dpkg --print-architecture | sed 's/^amd64$/x86_64/;s/^arm64$/arm64-v8a/')" \
 && yes | sdkmanager --licenses > /dev/null \
 && sdkmanager \
      "build-tools;${ANDROID_BUILD_TOOLS_VERSION}" \
      "emulator" \
      "ndk;${ANDROID_NDK_VERSION}" \
      "platform-tools" \
      "platforms;android-${ANDROID_PLATFORM_VERSION}" \
      "${image}" \
 && avdmanager create avd \
      --force \
      --name "${ANDROID_AVD}" \
      --package "${image}" \
      --device pixel_6 \
 && chown -R runner:runner "${ANDROID_USER_HOME}"

# Where the NDK landed. `cargo ndk` works this out from the SDK, but the build
# scripts of vendored C libraries do not: they read this variable and stop.
ENV ANDROID_NDK_HOME=${ANDROID_HOME}/ndk/${ANDROID_NDK_VERSION}

RUN --mount=type=secret,id=cache_env,required=true \
    --mount=type=secret,id=kache_host,required=true \
    --mount=type=cache,target=/tmp/kithara-kache/store \
    --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    . /run/secrets/cache_env; trap 'kache daemon stop >&2' EXIT; \
    kache stats --json | grep -Eq '"remote"[[:space:]]*:[[:space:]]*"s3://' && \
    CARGO_BUILD_JOBS=2 \
    cargo install --locked --version "${CARGO_NDK_VERSION}" cargo-ndk \
 && rustup target add \
      aarch64-linux-android \
      armv7-linux-androideabi \
      i686-linux-android \
      x86_64-linux-android
