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

"$lest" validate
"$lest" run plants-api
"$lest" run everything
"$lest" run connect-weather

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
echo "e2e: all example flows behaved as expected"
