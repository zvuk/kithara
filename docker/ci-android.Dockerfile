ARG CI_REGISTRY_IMAGE
FROM ${CI_REGISTRY_IMAGE}/ci:latest

ENV ANDROID_NDK_VERSION=r27c
ENV ANDROID_NDK_HOME=/opt/android-ndk

RUN apt-get update && apt-get install -y --no-install-recommends \
    unzip wget \
    && rm -rf /var/lib/apt/lists/* \
 && wget -q https://dl.google.com/android/repository/android-ndk-${ANDROID_NDK_VERSION}-linux.zip \
 && unzip -q android-ndk-*.zip -d /opt \
 && mv /opt/android-ndk-* $ANDROID_NDK_HOME \
 && rm android-ndk-*.zip

RUN cargo install cargo-ndk --locked \
 && rustup target add \
    aarch64-linux-android \
    armv7-linux-androideabi \
    x86_64-linux-android \
 && rm -rf /usr/local/cargo/registry /usr/local/cargo/git
