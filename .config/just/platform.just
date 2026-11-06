set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

# WASM artifacts: check, build, size-check, or doc. The browser tests are Rust
# tests and live in `just test wasm`.
[positional-arguments]
wasm MODE="check" *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    mode=$1
    shift
    case "$mode" in
      check)
        # `+atomics` already forces `-Z build-std` on the browser test lane, and
        # `parking_lot_core` needs its nightly-only feature to compile a wasm
        # parker that does not panic. The target is nightly-only either way.
        toolchain="${KITHARA_NIGHTLY_TOOLCHAIN:-nightly}"
        for c in kithara-storage kithara-net kithara-download kithara-stream kithara-assets kithara-hls kithara-command; do
          cargo "+$toolchain" check -p "$c" --target wasm32-unknown-unknown
        done
        cargo "+$toolchain" check -p kithara-beat --target wasm32-unknown-unknown --features dsp
        cargo "+$toolchain" check -p kithara-decode --target wasm32-unknown-unknown --no-default-features --features symphonia,webcodecs,client-reqwest,tls-rustls
        cargo "+$toolchain" check -p kithara-analysis --target wasm32-unknown-unknown --no-default-features --features analysis-waveform,beat-dsp,client-reqwest,tls-rustls,symphonia,webcodecs
        cargo "+$toolchain" check -p kithara-play --target wasm32-unknown-unknown --no-default-features --features symphonia,webcodecs,resample-rubato,stretch-identity,client-reqwest,tls-rustls
        cargo "+$toolchain" check -p kithara-host --target wasm32-unknown-unknown --no-default-features --features offline,symphonia,webcodecs,resample-rubato,stretch-identity,client-reqwest,tls-rustls
        cargo "+$toolchain" check -p kithara-ui --lib --target wasm32-unknown-unknown
        cargo "+$toolchain" check -p kithara-app --lib --target wasm32-unknown-unknown --no-default-features --features gui,analysis-waveform,beat-dsp,stretch-identity
        cargo "+$toolchain" check -p kithara-app --bin kithara --target wasm32-unknown-unknown --no-default-features --features gui,analysis-waveform,beat-dsp,stretch-identity
        cargo "+$toolchain" check -p kithara-app --bin kithara --target wasm32-unknown-unknown --no-default-features --features gui,analysis-waveform,beat-dsp,stretch-identity,zvuk
        cargo "+$toolchain" check -p kithara-ffi --target wasm32-unknown-unknown --features wasm --no-default-features
        ;;
      build)
        just _xtask wasm build "$@"
        ;;
      size-check)
        just _xtask wasm size-check "$@"
        ;;
      doc)
        just _xtask wasm doc
        ;;
      *)
        echo "unknown wasm mode: $mode (use check|build|size-check|doc); tests are in just test wasm"
        exit 2
        ;;
    esac

# Android tasks: build, aar, doc, or another xtask mode.
[positional-arguments]
android MODE="build" *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    mode=$1
    shift
    case "$mode" in
      build)
        just _xtask android build "$@"
        ;;
      aar)
        just _xtask android aar
        ;;
      doc)
        just _xtask android doc "$@"
        ;;
      *)
        just _xtask android "$mode" "$@"
        ;;
    esac

# Apple commands are owned by xtask.
[positional-arguments]
apple MODE="xcframework" *ARGS:
    just _xtask apple "$@"
