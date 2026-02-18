#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
COMPARE_SCRIPT="$SCRIPT_DIR/compare-perf.sh"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

baseline="$tmpdir/baseline.txt"
current_ok="$tmpdir/current-ok.txt"
current_bad="$tmpdir/current-bad.txt"
current_mismatch="$tmpdir/current-mismatch.txt"

cat >"$baseline" <<'EOF'
| Function | Calls | Avg | P95 | Total | % Total |
| decode_chunk | 1000 | 1.00ms | 1.20ms | 1.00s | 50% |
| parse_header | 1000 | 200.00us | 250.00us | 0.20s | 50% |
EOF

cat >"$current_ok" <<'EOF'
| Function | Calls | Avg | P95 | Total | % Total |
| decode_chunk | 1000 | 1.05ms | 1.25ms | 1.05s | 50% |
| parse_header | 1000 | 190.00us | 240.00us | 0.19s | 50% |
EOF

cat >"$current_bad" <<'EOF'
| Function | Calls | Avg | P95 | Total | % Total |
| decode_chunk | 1000 | 1.50ms | 1.90ms | 1.50s | 50% |
| parse_header | 1000 | 205.00us | 250.00us | 0.21s | 50% |
EOF

cat >"$current_mismatch" <<'EOF'
| Function | Calls | Avg | P95 | Total | % Total |
| unrelated_fn | 1000 | 50.00us | 60.00us | 0.05s | 100% |
EOF

"$COMPARE_SCRIPT" "$current_ok" "$baseline" 10 >/dev/null

if "$COMPARE_SCRIPT" "$current_bad" "$baseline" 10 >/dev/null; then
    echo "Expected regression check to fail"
    exit 1
fi

if "$COMPARE_SCRIPT" "$current_mismatch" "$baseline" 10 >/dev/null; then
    echo "Expected mismatch check to fail"
    exit 1
fi

echo "compare-perf.sh tests passed"
