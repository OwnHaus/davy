// Writes a Brotli copy next to each text file of a UI build, then fails when
// the script a first visit loads is over its budget once compressed.
// Usage: node scripts/ui-pack.mjs <build directory>

import { readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join, relative } from "node:path";
import { brotliCompressSync, constants } from "node:zlib";

const BUDGET = 200 * 1024;
const TEXT = /\.(html|js|mjs|css|svg|json|txt)$/;

const dir = process.argv[2];
if (!dir) {
  console.error("usage: node scripts/ui-pack.mjs <build directory>");
  process.exit(2);
}

const files = readdirSync(dir, { recursive: true, withFileTypes: true })
  .filter((e) => e.isFile() && TEXT.test(e.name))
  .map((e) => join(e.parentPath, e.name));

const packed = new Map();
for (const file of files) {
  const bytes = brotliCompressSync(readFileSync(file), {
    params: { [constants.BROTLI_PARAM_QUALITY]: constants.BROTLI_MAX_QUALITY },
  });
  writeFileSync(`${file}.br`, bytes);
  packed.set(relative(dir, file), bytes.length);
}

// The page loads its entry module and every module it preloads. Lazy chunks
// load later and count against nothing here.
const page = readFileSync(join(dir, "index.html"), "utf8");
const loaded = [...page.matchAll(/<(?:script|link)\b[^>]*?(?:src|href)="\/ui\/([^"]+\.js)"/g)].map((m) => m[1]);
if (loaded.length === 0) {
  console.error("ui-pack: index.html loads no script, so the budget check would pass for nothing");
  process.exit(1);
}
let total = 0;
for (const name of loaded) {
  const size = packed.get(name);
  if (size === undefined) {
    console.error(`ui-pack: index.html loads ${name}, which the build does not have`);
    process.exit(1);
  }
  total += size;
}
const kb = (n) => `${(n / 1024).toFixed(1)} KB`;
console.log(`ui-pack: first visit loads ${kb(total)} of script with Brotli, the budget is ${kb(BUDGET)}`);
if (total > BUDGET) {
  console.error("ui-pack: over budget. Load the heavy part lazily or drop a dependency.");
  process.exit(1);
}
