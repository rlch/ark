#!/usr/bin/env bash
# wasm-size-check.sh — compare wasm plugin sizes against a recorded baseline.
#
# Usage:
#   scripts/wasm-size-check.sh <baseline_csv> <current_csv> [growth_threshold_pct]
#
# Inputs (both CSVs share the same format, produced by the `wasm-build` job in
# .github/workflows/ci.yml):
#   plugin,bytes
#   ark_plugin_status,523412
#   ark_plugin_picker,612339
#
# Behavior:
#   * For every plugin that appears in the baseline, look up the matching row
#     in the current CSV. Compute delta_bytes and delta_pct.
#   * Print a human-readable table with per-plugin baseline, current, delta,
#     and delta%.
#   * Exit 1 if any plugin grew by more than <growth_threshold_pct> percent
#     (default: 25). Negative or zero growth is always allowed.
#   * A baseline row whose size is "MISSING" is reported but does not cause
#     a failure — the current build wins by default.
#   * A plugin present in the baseline but absent from the current CSV is a
#     hard failure (the gate wants a regression signal, not a silent drop).
#
# This script is pure POSIX bash + awk + sort — no jq, no python — so it runs
# unchanged on ubuntu-latest and macos-latest GitHub runners.
#
# Related spec: cavekit-distribution.md R3 ("CI regression check fails the PR
# if either plugin grows >25% vs main"). Build-site task: T-132.

set -euo pipefail

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  echo "usage: $0 <baseline_csv> <current_csv> [growth_threshold_pct]" >&2
  exit 2
fi

baseline_csv="$1"
current_csv="$2"
threshold_pct="${3:-25}"

if [ ! -f "$baseline_csv" ]; then
  echo "wasm-size-check: baseline CSV not found: $baseline_csv" >&2
  exit 2
fi
if [ ! -f "$current_csv" ]; then
  echo "wasm-size-check: current CSV not found: $current_csv" >&2
  exit 2
fi

# Resolve a plugin's size from a CSV file. Prints "MISSING" if the plugin has
# no row, echoes the recorded string (either a byte count or the literal
# "MISSING" sentinel the CI job writes when a .wasm is absent) otherwise.
lookup_size() {
  local csv="$1" plugin="$2"
  awk -F',' -v p="$plugin" '
    NR == 1 { next }            # skip header row
    $1 == p { print $2; found = 1; exit }
    END { if (!found) print "MISSING" }
  ' "$csv"
}

# Emit a formatted table row with fixed-width columns so the CI log stays
# readable when GitHub wraps wide lines.
print_header() {
  printf "%-24s  %12s  %12s  %12s  %8s\n" \
    "plugin" "baseline" "current" "delta" "delta%"
  printf "%-24s  %12s  %12s  %12s  %8s\n" \
    "------------------------" "------------" "------------" "------------" "--------"
}

print_row() {
  # $1 plugin  $2 baseline  $3 current  $4 delta  $5 delta_pct
  printf "%-24s  %12s  %12s  %12s  %8s\n" "$1" "$2" "$3" "$4" "$5"
}

# Collect plugin names from the baseline, skipping the header.
plugins=$(awk -F',' 'NR > 1 && $1 != "" { print $1 }' "$baseline_csv")

if [ -z "$plugins" ]; then
  echo "wasm-size-check: baseline CSV has no plugin rows (only header?)" >&2
  exit 2
fi

print_header

fail=0
while IFS= read -r plugin; do
  [ -z "$plugin" ] && continue
  base_size=$(lookup_size "$baseline_csv" "$plugin")
  curr_size=$(lookup_size "$current_csv" "$plugin")

  # Missing baseline → informational only: print and move on.
  if [ "$base_size" = "MISSING" ]; then
    print_row "$plugin" "MISSING" "$curr_size" "n/a" "n/a"
    continue
  fi

  # Missing current build for a plugin we had before → hard fail.
  if [ "$curr_size" = "MISSING" ]; then
    print_row "$plugin" "$base_size" "MISSING" "n/a" "n/a"
    echo "wasm-size-check: FAIL — plugin '$plugin' is missing from current build" >&2
    fail=1
    continue
  fi

  # Both sides are numeric. Use awk for the arithmetic (handles the percentage
  # as a float and keeps us portable — bash has no float math).
  read -r delta delta_pct <<EOF
$(awk -v b="$base_size" -v c="$curr_size" 'BEGIN {
    d = c - b;
    if (b == 0) { pct = 0 } else { pct = (d / b) * 100 }
    printf "%d %.2f", d, pct;
  }')
EOF

  print_row "$plugin" "$base_size" "$curr_size" "$delta" "${delta_pct}%"

  # Compare delta_pct > threshold. Use awk again so we don't fight bash's lack
  # of floating-point comparison.
  exceeds=$(awk -v p="$delta_pct" -v t="$threshold_pct" 'BEGIN { print (p > t) ? "1" : "0" }')
  if [ "$exceeds" = "1" ]; then
    echo "wasm-size-check: FAIL — '$plugin' grew ${delta_pct}% (> ${threshold_pct}% threshold)" >&2
    fail=1
  fi
done <<< "$plugins"

echo
if [ "$fail" -ne 0 ]; then
  echo "wasm-size-check: one or more plugins exceeded the ${threshold_pct}% growth threshold." >&2
  exit 1
fi

echo "wasm-size-check: OK — all plugins within ${threshold_pct}% growth budget."
