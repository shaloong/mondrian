#!/usr/bin/env bash
# Architecture V2 metrics — run periodically to track migration progress.
# Usage: bash scripts/arch_metrics.sh [--json]
# Output: human-readable or JSON summary of architecture health indicators.

set -uo pipefail
cd "$(dirname "$0")/.."

JSON_OUTPUT=false
if [[ "${1:-}" == "--json" ]]; then
    JSON_OUTPUT=true
fi

# ── Dependency violation checks ──────────────────────────────────────────

# P-ARCH1: Timeline must not depend on Effects (check [dependencies] only, not [dev-dependencies])
TIMELINE_DEPS_EFFECTS=$(awk '/^\[dependencies\]/{found=1} /^\[/{if($0!="[dependencies]") found=0} found && /mondrian-effects/{print}' crates/mondrian-timeline/Cargo.toml 2>/dev/null | wc -l | tr -d ' ')

# P-ARCH2: Renderer must not depend on Timeline
RENDERER_DEPS_TIMELINE=$(awk '/^\[dependencies\]/{found=1} /^\[/{if($0!="[dependencies]") found=0} found && /mondrian-timeline/{print}' crates/mondrian-renderer/Cargo.toml 2>/dev/null | wc -l | tr -d ' ')

# ── Global mutable state count ────────────────────────────────────────────

GLOBAL_ONCELOCK_COUNT=$(grep -r "OnceLock" crates/ --include="*.rs" \
    | grep -v "^\s*//" | grep -v "#\[cfg(test)\]" \
    | wc -l | tr -d ' ')

GLOBAL_STATIC_MUTEX_COUNT=$(grep -r "static.*Mutex" crates/ --include="*.rs" \
    | grep -v "^\s*//" | grep -v "#\[cfg(test)\]" \
    | wc -l | tr -d ' ')

# ── File size health ─────────────────────────────────────────────────────

FILES_OVER_1000=$(find crates/ -name "*.rs" -exec wc -l {} \; \
    | awk '$1 > 1000 { print $1 }' | wc -l | tr -d ' ')
FILES_OVER_500=$(find crates/ -name "*.rs" -exec wc -l {} \; \
    | awk '$1 > 500 { print $1 }' | wc -l | tr -d ' ')

# Identify the worst offenders
WORST_FILES=$(find crates/ -name "*.rs" -exec wc -l {} \; \
    | sort -rn | head -5 | awk '{print $2 " (" $1 " lines)"}')

# ── Test coverage ─────────────────────────────────────────────────────────

TEST_COUNT=$(grep -r "#\[test\]" crates/ --include="*.rs" | wc -l | tr -d ' ')
CRATES_WITHOUT_TESTS=""
for crate_dir in crates/*/; do
    crate_name=$(basename "$crate_dir")
    test_count=$(grep -r "#\[test\]" "$crate_dir" --include="*.rs" 2>/dev/null | wc -l | tr -d ' ')
    if [ "$test_count" -eq 0 ]; then
        CRATES_WITHOUT_TESTS="$CRATES_WITHOUT_TESTS $crate_name"
    fi
done

INTEGRATION_TEST_COUNT=$(find crates/ -path "*/tests/*.rs" -exec grep -c "#\[test\]" {} \; \
    2>/dev/null | awk '{s+=$1} END {print s+0}')

# ── Dead WGSL shaders ────────────────────────────────────────────────────

DEAD_SHADERS=""
for shader in crates/mondrian-renderer/shaders/*.wgsl; do
    shader_name=$(basename "$shader" .wgsl)
    ref_count=$(grep -r "$shader_name" crates/mondrian-renderer/src/ --include="*.rs" \
        | grep -v "shaders\.rs" | wc -l | tr -d ' ')
    if [ "$ref_count" -eq 0 ]; then
        DEAD_SHADERS="$DEAD_SHADERS $shader_name"
    fi
done

# ── Unsafe blocks ─────────────────────────────────────────────────────────

UNSAFE_COUNT=$(grep -r "unsafe" crates/ --include="*.rs" \
    | grep -v "^\s*//" | grep -v "#\[cfg(test)\]" \
    | wc -l | tr -d ' ')

# ── Unwrap calls in non-test code ─────────────────────────────────────────

UNWRAP_COUNT=$(grep -r "\.unwrap()" crates/ --include="*.rs" \
    | grep -v "^\s*//" | grep -v "#\[cfg(test)\]" | grep -v "tests/" \
    | wc -l | tr -d ' ')

# ── Output ────────────────────────────────────────────────────────────────

if $JSON_OUTPUT; then
    cat <<EOF
{
  "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "dependency_violations": {
    "timeline_depends_on_effects": $TIMELINE_DEPS_EFFECTS,
    "renderer_depends_on_timeline": $RENDERER_DEPS_TIMELINE
  },
  "global_state": {
    "oncelock_instances": $GLOBAL_ONCELOCK_COUNT,
    "static_mutex_instances": $GLOBAL_STATIC_MUTEX_COUNT
  },
  "file_health": {
    "files_over_1000_lines": $FILES_OVER_1000,
    "files_over_500_lines": $FILES_OVER_500
  },
  "test_coverage": {
    "total_tests": $TEST_COUNT,
    "integration_tests": $INTEGRATION_TEST_COUNT,
    "crates_without_tests": "$(echo $CRATES_WITHOUT_TESTS | xargs)"
  },
  "dead_shaders": "$(echo $DEAD_SHADERS | xargs)",
  "unsafe_blocks": $UNSAFE_COUNT,
  "non_test_unwraps": $UNWRAP_COUNT
}
EOF
else
    echo "══════════════════════════════════════════════"
    echo "  Mondrian Architecture V2 Health Report"
    echo "══════════════════════════════════════════════"
    echo ""
    echo "── Dependency Violations ──"
    echo "  Timeline → Effects:  $(if [ "$TIMELINE_DEPS_EFFECTS" -gt 0 ]; then echo "VIOLATED ($TIMELINE_DEPS_EFFECTS refs)"; else echo "CLEAN"; fi)"
    echo "  Renderer → Timeline: $(if [ "$RENDERER_DEPS_TIMELINE" -gt 0 ]; then echo "VIOLATED ($RENDERER_DEPS_TIMELINE refs)"; else echo "CLEAN"; fi)"
    echo ""
    echo "── Global Mutable State ──"
    echo "  OnceLock instances:   $GLOBAL_ONCELOCK_COUNT"
    echo "  Static Mutex instances: $GLOBAL_STATIC_MUTEX_COUNT"
    echo ""
    echo "── File Health ──"
    echo "  Files > 1000 lines: $FILES_OVER_1000"
    echo "  Files > 500 lines:  $FILES_OVER_500"
    echo "  Worst offenders:"
    echo "$WORST_FILES" | while read -r line; do echo "    $line"; done
    echo ""
    echo "── Test Coverage ──"
    echo "  Total unit tests:     $TEST_COUNT"
    echo "  Integration tests:    $INTEGRATION_TEST_COUNT"
    echo "  Crates without tests:$(if [ -z "$CRATES_WITHOUT_TESTS" ]; then echo " NONE"; else echo "$CRATES_WITHOUT_TESTS"; fi)"
    echo ""
    echo "── Shader Health ──"
    echo "  Dead WGSL shaders:$(if [ -z "$DEAD_SHADERS" ]; then echo " NONE"; else echo "$DEAD_SHADERS"; fi)"
    echo ""
    echo "── Safety ──"
    echo "  unsafe blocks:   $UNSAFE_COUNT"
    echo "  non-test unwraps: $UNWRAP_COUNT"
    echo ""
    echo "══════════════════════════════════════════════"
fi
