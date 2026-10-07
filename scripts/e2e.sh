#!/usr/bin/env bash
# Runs the example flows against the bundled sample app with a release build.
# Needs Node and Playwright's Chromium (see docs/browser.md).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
lest="${LEST_BIN:-$root/target/release/lest}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export LEST_DATA_DIR="$work/data" LEST_CONFIG_DIR="$work/config"
cd "$root/examples"

"$lest" doctor || true
"$lest" validate
"$lest" run plants-api
"$lest" run everything
"$lest" run connect-weather -o "$work/demo.json"
node -e '
  const r = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
  if (!r.demo || !r.demo.video) throw new Error("no demo video: " + JSON.stringify(r.demo));
  if (r.demo.chapters.length !== 3) throw new Error("expected 3 chapters, got " + r.demo.chapters.length);
  if (!(r.demo.durationMs < r.demo.rawDurationMs)) throw new Error("the cut is not shorter than the take");
' "$work/demo.json"

# The failure example must fail, with the failing page step named.
set +e
"$lest" run wrong-password -o "$work/failed.json"
code=$?
set -e
if [ "$code" -ne 1 ]; then
  echo "e2e: wrong-password exited $code, expected 1" >&2
  exit 1
fi
node -e '
  const r = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
  const step = r.steps.find((s) => s.id === "page_login");
  if (!/never appeared/.test(step.headline)) throw new Error("unexpected headline: " + step.headline);
  if (!step.artifacts.some((a) => a.label === "Screenshot at failure")) throw new Error("no failure screenshot");
' "$work/failed.json"
# Raw browser event files can hold unredacted values; none may remain.
if find "$LEST_DATA_DIR" -name '*.events.jsonl' | grep -q .; then
  echo "e2e: browser event files were left in run directories" >&2
  exit 1
fi
echo "e2e: all example flows behaved as expected"
