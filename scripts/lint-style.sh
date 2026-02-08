#!/usr/bin/env bash
# Style linter for the kithara codebase.
# Catches patterns that rustfmt/clippy cannot enforce.
# Run: bash scripts/lint-style.sh

set -euo pipefail

errors=0
warnings=0

# 1. No separator comments (// ====... or // ────...)
while IFS= read -r line; do
    echo "ERROR: separator comment: $line"
    errors=$((errors + 1))
done < <(grep -rn '// [=─]\{10,\}' --include='*.rs' crates/ tests/ || true)

# 2. No inline std:: qualified paths in function bodies (advisory)
#    Excludes: use statements, macro_rules!, comments, derive attributes
INLINE_PATHS=(
    'std::io::'
    'std::fmt::'
    'std::error::'
    'std::ops::'
    'std::sync::atomic::'
    'std::collections::'
    'tokio::sync::'
    'tokio::time::'
    'tokio::task::'
)

for pattern in "${INLINE_PATHS[@]}"; do
    while IFS= read -r line; do
        # Skip use statements
        if echo "$line" | grep -q '^\s*[0-9]*[:-]\s*use '; then continue; fi
        # Skip comments and doc comments
        if echo "$line" | grep -q '^\s*[0-9]*[:-]\s*//'; then continue; fi
        # Skip derive attributes
        if echo "$line" | grep -q '#\[derive'; then continue; fi
        # Skip macro_rules
        if echo "$line" | grep -q 'macro_rules!'; then continue; fi

        warnings=$((warnings + 1))
    done < <(grep -rn "$pattern" --include='*.rs' crates/ tests/ || true)
done

if [ "$warnings" -gt 0 ]; then
    echo "WARNING: $warnings inline qualified path(s) found (advisory, not blocking)."
fi

if [ "$errors" -gt 0 ]; then
    echo ""
    echo "FAILED: $errors style violation(s) found."
    exit 1
else
    echo "OK: no blocking style violations."
    exit 0
fi
