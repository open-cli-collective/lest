#!/usr/bin/env bash
# Scans tracked files (and optionally extra text on stdin) against a
# maintainer-private list of terms that must never be published.
# The list lives outside the repository: $LEST_DENYLIST_FILE or
# ~/.config/lest-dev/denylist, one extended regex per line, '#' comments.
set -euo pipefail
list="${LEST_DENYLIST_FILE:-$HOME/.config/lest-dev/denylist}"
if [ ! -f "$list" ]; then
  echo "check-denylist: no list at $list, skipping" >&2
  exit 0
fi
patterns="$(mktemp)"
trap 'rm -f "$patterns"' EXIT
grep -vE '^\s*(#|$)' "$list" > "$patterns"
status=0
if [ "${1:-}" = "--stdin" ]; then
  if grep -inE -f "$patterns" -; then status=1; fi
else
  # Binary files are scanned too (images can carry text in metadata).
  files="$(git ls-files)"
  if [ -n "$files" ] && git ls-files -z | xargs -0 grep -inE -f "$patterns" --; then status=1; fi
  if git ls-files | grep -iE -f "$patterns"; then status=1; fi
fi
if [ "$status" -ne 0 ]; then
  echo "check-denylist: matches found" >&2
fi
exit "$status"
