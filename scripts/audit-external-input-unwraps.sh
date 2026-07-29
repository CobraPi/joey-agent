#!/usr/bin/env bash
#
# audit-external-input-unwraps.sh — FR-010 audit script for feature
# 006-bug-sweep-clippy-panic-hardening.
#
# Enumerates `.unwrap()` / `.expect()` / `panic!()` / `unreachable!()` call
# sites in `src/` (excluding `tests/` and `#[cfg(test)]`) across the 7
# in-scope crates, classifies each as external-input vs. safe, prints a
# per-crate breakdown, and exits non-zero if any external-input site lacks a
# typed-error conversion or `// SAFETY:` / `// invariant:` comment.
#
# Classification heuristic (research.md R4):
#   A site is "external-input" if its enclosing file path matches one of the
#   curated external-input patterns below. Otherwise it is "safe".
#   - Safe-tier retained sites should carry a `// SAFETY:` or `// invariant:`
#     comment on the preceding non-blank line (FR-007). Lack of it is reported
#     as informational, not a hard failure.
#   - External-input sites must be hardened (converted to typed errors /
#     logged fallbacks); their presence is a failure.
#
# Inline #[cfg(test)] modules are excluded by finding the line number of each
# `#[cfg(test)]` attribute and skipping all lines at or after it in the same
# file. This is correct because Rust convention places test modules at the
# end of the file (after all production code), and the attribute always
# precedes the `mod tests {` block.
#
# Exit codes:
#   0 — no unhardened external-input sites found
#   1 — one or more external-input sites lack hardening
#
# Dependencies: bash, rg (ripgrep), grep, sed, awk. No new runtime dependency
# (Constitution VIII). Compatible with bash 3.2 (macOS default).
#
set -u

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CRATES="joey-tools joey-providers joey-core joey-mcp joey-gateway joey-cron joey-agent-core"

# Curated external-input file patterns per crate (space-separated globs).
# Sites in files matching these patterns are classified "external-input".
ext_patterns_for() {
    case "$1" in
        joey-tools)        echo "src/sanitize src/sanitize_input src/lsp.rs src/tools/file_tools.rs src/tools/session_search_tool.rs src/safe_commands.rs" ;;
        joey-providers)    echo "src/stream src/response src/request src/sse src/parse src/client src/chat src/responses src/openai src/anthropic" ;;
        joey-core)         echo "src/config.rs src/auth_store.rs src/session src/store src/redact.rs src/paths.rs src/lib.rs" ;;
        joey-mcp)          echo "src/security.rs src/config.rs src/schema.rs src/lib.rs src/result.rs" ;;
        joey-gateway)      echo "src/" ;;
        joey-cron)         echo "src/job src/store src/parse src/schedule src/lib.rs" ;;
        joey-agent-core)   echo "src/agent.rs src/prompt.rs src/compression src/turn src/loop src/hooks.rs src/verification.rs" ;;
        *)                 echo "" ;;
    esac
}

PANIC_PAT='\.unwrap\(\)|\.expect\(|panic!\(|unreachable!\('

total_external=0
total_safe_uncommented=0
total_safe_commented=0
failed=0

printf '%s\n' "==============================================="
printf '  External-Input Unwrap/Expect/Panic Audit\n'
printf '  Feature: 006-bug-sweep-clippy-panic-hardening\n'
printf '%s\n' "==============================================="
printf '\n'

for crate in $CRATES; do
    crate_dir="$REPO_ROOT/crates/$crate"
    if [ ! -d "$crate_dir" ]; then
        printf '  (skip) %s: directory not found\n' "$crate"
        continue
    fi

    patterns="$(ext_patterns_for "$crate")"
    crate_external=0
    crate_safe_commented=0
    crate_safe_uncommented=0
    crate_fail_sites=""

    # Gather all matching sites in src/ excluding tests/ and #[cfg(test)].
    while IFS= read -r line; do
        # line format: filepath:lineno:content
        file="${line%%:*}"
        rest="${line#*:}"
        lineno="${rest%%:*}"
        content="${rest#*:}"

        # Skip lines inside #[cfg(test)] inline modules.
        # Find the first #[cfg(test)] line in this file; if our line is at
        # or after it, skip (test modules are at file end in Rust convention).
        cfg_test_line=$(grep -n '#\[cfg(test)\]' "$file" 2>/dev/null | head -1 | cut -d: -f1)
        if [ -n "$cfg_test_line" ] && [ "$lineno" -ge "$cfg_test_line" ] 2>/dev/null; then
            continue
        fi

        # Skip standalone test files (*_tests.rs / *_test.rs) that are
        # declared under #[cfg(test)] in their parent mod.rs.
        basename=$(basename "$file")
        case "$basename" in
            *_tests.rs|*_test.rs)
                mod_name="${basename%.rs}"
                parent_dir=$(dirname "$file")
                if grep -q "#\[cfg(test)\]" "$parent_dir/mod.rs" 2>/dev/null && \
                   grep -q "mod $mod_name;" "$parent_dir/mod.rs" 2>/dev/null; then
                    continue
                fi
                ;;
        esac

        # Determine if this file matches an external-input pattern.
        is_external=0
        if [ -n "$patterns" ]; then
            for pat in $patterns; do
                case "$file" in
                    *"$pat"*) is_external=1; break ;;
                esac
            done
        fi

        if [ $is_external -eq 1 ]; then
            # External-input site: check if it has a SAFETY comment that
            # reclassifies it as safe (FR-007). Check preceding non-blank
            # line, the same line (inline comment), and the next line.
            has_safety=0
            # Check same line for trailing SAFETY comment.
            if printf '%s' "$content" | grep -qiE '// (SAFETY|invariant):'; then
                has_safety=1
            fi
            # Check preceding lines (walk back through anything, up to 15
            # lines, to find a SAFETY comment associated with this call).
            if [ $has_safety -eq 0 ] && [ "$lineno" -gt 1 ]; then
                prev_no=$((lineno - 1))
                steps=0
                while [ $steps -lt 15 ] && [ "$prev_no" -ge 1 ]; do
                    prev_line=$(sed -n "${prev_no}p" "$file" 2>/dev/null || true)
                    if printf '%s' "$prev_line" | grep -qiE '// (SAFETY|invariant):'; then
                        has_safety=1
                        break
                    fi
                    prev_no=$((prev_no - 1))
                    steps=$((steps + 1))
                done
            fi
            # Check the next non-blank line (SAFETY comment after the call).
            if [ $has_safety -eq 0 ]; then
                next_no=$((lineno + 1))
                next_check=0
                while [ $next_check -lt 3 ]; do
                    next_line=$(sed -n "${next_no}p" "$file" 2>/dev/null || true)
                    trimmed="$(printf '%s' "$next_line" | tr -d '[:space:]')"
                    if [ -z "$trimmed" ]; then
                        next_no=$((next_no + 1))
                        next_check=$((next_check + 1))
                        continue
                    fi
                    if printf '%s' "$next_line" | grep -qiE '// (SAFETY|invariant):'; then
                        has_safety=1
                    fi
                    break
                done
            fi
            if [ $has_safety -eq 1 ]; then
                crate_safe_commented=$((crate_safe_commented + 1))
            else
                crate_external=$((crate_external + 1))
                crate_fail_sites="$crate_fail_sites
    $file:$lineno"
            fi
        else
            # Safe-tier: check preceding non-blank line for SAFETY comment.
            has_safety=0
            if [ "$lineno" -gt 1 ]; then
                prev_no=$((lineno - 1))
                prev_line=$(sed -n "${prev_no}p" "$file" 2>/dev/null || true)
                # If blank, walk back up to 3 lines.
                blank_check=0
                while [ $blank_check -lt 3 ]; do
                    trimmed="$(printf '%s' "$prev_line" | tr -d '[:space:]')"
                    if [ -z "$trimmed" ] && [ "$prev_no" -gt 1 ]; then
                        prev_no=$((prev_no - 1))
                        prev_line=$(sed -n "${prev_no}p" "$file" 2>/dev/null || true)
                        blank_check=$((blank_check + 1))
                    else
                        break
                    fi
                done
                if printf '%s' "$prev_line" | grep -qiE '// (SAFETY|invariant):'; then
                    has_safety=1
                fi
            fi
            if [ $has_safety -eq 1 ]; then
                crate_safe_commented=$((crate_safe_commented + 1))
            else
                crate_safe_uncommented=$((crate_safe_uncommented + 1))
            fi
        fi
    done <<EOF
$(rg -n --no-heading "$PANIC_PAT" "$crate_dir/src" 2>/dev/null \
  | grep -v '/tests/' \
  || true)
EOF

    total_external=$((total_external + crate_external))
    total_safe_commented=$((total_safe_commented + crate_safe_commented))
    total_safe_uncommented=$((total_safe_uncommented + crate_safe_uncommented))

    if [ $crate_external -gt 0 ]; then
        failed=1
    fi

    printf '  %-22s  external=%-4d  safe+comment=%-4d  safe-uncommented=%-4d\n' \
        "$crate" "$crate_external" "$crate_safe_commented" "$crate_safe_uncommented"
    if [ -n "$crate_fail_sites" ] && [ $crate_external -gt 0 ]; then
        printf '    ⚠ unhardened external-input sites:%s\n' "$crate_fail_sites"
    fi
done

printf '\n%s\n' "-----------------------------------------------"
printf '  TOTAL external-input: %d\n' "$total_external"
printf '  TOTAL safe+comment:   %d\n' "$total_safe_commented"
printf '  TOTAL safe uncomment: %d (informational)\n' "$total_safe_uncommented"
printf '%s\n' "-----------------------------------------------"

if [ $total_external -gt 0 ]; then
    printf '\nFAIL: %d external-input site(s) still unhardened.\n' "$total_external"
    printf '   Harden each (typed error / logged fallback) per FR-005/FR-006/contracts.\n'
    exit 1
fi

printf '\nPASS: zero unhardened external-input sites.\n'
exit 0
