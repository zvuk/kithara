set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := "../.."

# Run all migrated session workloads through the canonical benchmark entrypoint.
[default]
run:
    just perf bench run --bench-target kithara-integration-tests:session_perf

# Per-widget frame time on both UI hosts, immediate and retained.
frames:
    just perf bench run --bench-target kithara-ui:frame_perf:iced,masonry,perf

# Whole-page frame time on both hosts: the control page and the bucket sweep.
pages:
    just perf bench run --bench-target kithara-ui-gallery:page_perf:masonry,perf --bench-filter 'gallery_(buttons|clock|table|stress)'

# Frame time on the pages that move on their own: posed objects, a sprite sheet
# and an artwork.
motion:
    just perf bench run --bench-target kithara-ui-gallery:page_perf:masonry,perf --bench-filter 'gallery_(objects|motion|sprites|lottie|scene)'

# Cost per wheel event against cost per frame, swept over wheels per frame.
scroll:
    just perf bench run --bench-target kithara-ui-gallery:page_perf:masonry,perf --bench-filter 'gallery_(pivot|library2|tree|table_long)|perf_scroll_vis'

# Cost per pointer move against cost per frame, with a fader held under the hand.
drag:
    just perf bench run --bench-target kithara-ui-gallery:page_perf:masonry,perf --bench-filter 'gallery_faders'

# The visualiser and document-shader passes, free and fenced (own scenario).
native:
    just perf bench run --bench-target kithara-ui-gallery:page_perf:masonry,perf --bench-filter 'gallery_(vis|shader)'

[positional-arguments]
suite *ARGS:
    just _xtask perf "$@"

[positional-arguments]
compare *ARGS:
    just _xtask perf-compare "$@"

# Linux: just perf bench dispatch linux --bench-target kithara-dsp:kernels:spectrum --bench-filter '<filter>' --bench-ref '<branch>' --bench-base-ref main
# Mac: just perf bench dispatch macos --bench-target kithara-dsp:kernels:spectrum --bench-filter '<filter>' --bench-ref '<branch>' --bench-base-ref main
[positional-arguments]
bench MODE="build" *ARGS:
    just _xtask perf bench "$@"

memory:
    @echo "=== Tier 1: Pool-native budget tests ==="
    cargo test -p kithara-bufpool --test bufpool memory_budget
    @echo ""
    @echo "=== Tier 2: Allocation regression tests ==="
    cargo test -p kithara-bufpool --test bufpool alloc_regression -- --test-threads=1
    @echo ""
    @echo "=== Tier 3: Audio hot path zero-alloc ==="
    cargo test -p kithara-audio-tests --test audio_heavy alloc_free_hotpath -- --test-threads=1
    @echo ""
    @echo "=== Tier 4: RSS budget (HLS playback) ==="
    cargo test -p kithara-integration-tests --features perf --test memory_rss -- --test-threads=1 --nocapture
    @echo ""
    @echo "All memory checks passed."
