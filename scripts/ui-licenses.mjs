// Fails when a package that ships in the UI bundle has a license outside the
// list in docs/design.md, "Front-end dependencies".
// Usage: pnpm licenses list --prod --json | node scripts/ui-licenses.mjs

const ALLOWED = new Set(["MIT", "ISC", "BSD-2-Clause", "BSD-3-Clause", "0BSD", "Apache-2.0", "Unlicense"]);

let input = "";
for await (const chunk of process.stdin) input += chunk;
const byLicense = JSON.parse(input);

let bad = 0;
for (const [license, packages] of Object.entries(byLicense)) {
  if (ALLOWED.has(license)) continue;
  for (const p of packages) console.error(`ui-licenses: ${p.name} ${p.versions.join(", ")} is ${license}`);
  bad += packages.length;
}
if (bad > 0) {
  console.error("ui-licenses: a bundled package has a license outside the list, drop it or add the license to docs/design.md first");
  process.exit(1);
}
