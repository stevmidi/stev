#!/usr/bin/env bash
# Advisory PostToolUse hook: after a Rust file is written/edited, flag any
# newly-added lines that use an inline multi-segment path (e.g.
# `egui::Rect::from_min_max`, `core::config::TRACK_COUNT`, `crate::a::b::c`)
# instead of a `use` at the top of the file. Convention:
# docs/080-conventions.md § Imports. Never blocks — it only feeds a
# reminder back so the model re-checks before finishing.

command -v jq >/dev/null 2>&1 || exit 0

input=$(cat)
f=$(printf '%s' "$input" | jq -r '.tool_input.file_path // empty')
[ -n "$f" ] || exit 0
case "$f" in *.rs) ;; *) exit 0 ;; esac
[ -f "$f" ] || exit 0

repo=$(git -C "$(dirname "$f")" rev-parse --show-toplevel 2>/dev/null)
[ -n "$repo" ] || exit 0

# Only inspect lines changed in this working session (vs the last commit).
added=$(git -C "$repo" diff HEAD --unified=0 -- "$f" 2>/dev/null \
  | grep -E '^\+[^+]' | sed 's/^+//')
if [ -z "$added" ] && ! git -C "$repo" ls-files --error-unmatch "$f" >/dev/null 2>&1; then
  added=$(cat "$f")   # brand-new untracked file
fi
[ -n "$added" ] || exit 0

# 3+ path segments, lowercase module head; skip comments, attributes, `use`
# lines, and the sanctioned 3-segment `std::` helper calls (std::mem::swap …).
hits=$(printf '%s\n' "$added" \
  | grep -vE '^[[:space:]]*(//|#\[|use )' \
  | grep -E '[a-z][a-z0-9_]*(::[A-Za-z_][A-Za-z0-9_]*){2,}' \
  | grep -vE 'std::[a-z_]+::[a-z_]+\(' \
  | sed 's/^[[:space:]]*//' | sort -u)
[ -n "$hits" ] || exit 0

rel=${f#"$repo"/}
msg=$(printf 'Import convention (%s): newly-added lines use inline multi-segment paths. Add a `use` at the top of the file and reference the short name, or confirm it matches the surrounding file on purpose — see docs/080-conventions.md § Imports.\n%s' "$rel" "$hits")

jq -n --arg m "$msg" '{
  systemMessage: $m,
  hookSpecificOutput: { hookEventName: "PostToolUse", additionalContext: $m }
}'
exit 0
