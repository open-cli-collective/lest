// Every external action in .github/workflows and action.yml must be pinned to
// a full commit SHA.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";

const dir = ".github/workflows";
const files = [...readdirSync(dir).filter((f) => /\.ya?ml$/.test(f)).map((f) => join(dir, f)), "action.yml"];
let bad = 0;
for (const file of files) {
  const lines = readFileSync(file, "utf8").split("\n");
  lines.forEach((line, i) => {
    const m = line.match(/^\s*-?\s*uses:\s*([^\s#]+)/);
    if (!m || m[1].startsWith("./")) return;
    if (!/@[0-9a-f]{40}$/.test(m[1])) {
      console.error(`${file}:${i + 1}: ${m[1]} is not pinned to a commit SHA`);
      bad++;
    }
  });
}
if (bad) process.exit(1);
console.log("workflows: every action is pinned");
