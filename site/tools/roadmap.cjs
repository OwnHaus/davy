// Checks roadmap.json and writes it into roadmap.html and the landing page
// between marker comments, so the site reads without JavaScript.
// Run: node site/tools/roadmap.cjs           rewrites the pages
//      node site/tools/roadmap.cjs --check   fails when a page is out of date
const fs = require('fs');
const path = require('path');

const root = path.join(__dirname, '..', '..');
const site = path.join(__dirname, '..');
const read = (p) => fs.readFileSync(p, 'utf8');

const STATUSES = ['in progress', 'next', 'later', 'shipped'];
const MAX_IN_PROGRESS = 3;
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

const errors = [];
const bad = (msg) => errors.push(msg);

function validate(doc, changelog) {
  if (!doc || typeof doc !== 'object' || Array.isArray(doc)) return bad('the file is not a JSON object');
  for (const k of Object.keys(doc)) if (!['about', 'items'].includes(k)) bad(`unknown top-level key "${k}"`);
  if (!Array.isArray(doc.items) || doc.items.length === 0) return bad('"items" is not a list of items');

  // Release headings, as scripts/release writes them: "## 0.1.0 (2026-11-23)".
  const releases = new Map();
  for (const m of changelog.matchAll(/^## (\d+\.\d+\.\d+) \((\d{4}-\d{2}-\d{2})\)$/gm)) releases.set(m[1], m[2]);
  const hasUnreleased = /^## Unreleased$/m.test(changelog);
  const lastRelease = [...releases.values()].sort().pop() || '0000-00-00';
  const now = new Date();
  const today = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;

  const ids = new Set();
  doc.items.forEach((it, n) => {
    const at = `item ${n + 1}${it && typeof it.id === 'string' ? ` (${it.id})` : ''}`;
    if (!it || typeof it !== 'object' || Array.isArray(it)) return bad(`${at} is not an object`);
    for (const k of Object.keys(it)) {
      if (!['id', 'title', 'what', 'status', 'version', 'date'].includes(k)) bad(`${at} has an unknown key "${k}"`);
    }
    if (typeof it.id !== 'string' || !/^[a-z0-9]+(-[a-z0-9]+)*$/.test(it.id)) bad(`${at}: "id" is not lower-case words joined by -`);
    else if (ids.has(it.id)) bad(`${at}: the id is used twice`);
    else ids.add(it.id);
    if (typeof it.title !== 'string' || !it.title.trim() || it.title.length > 60 || /[.]$/.test(it.title)) {
      bad(`${at}: "title" is 1 to 60 characters without a full stop`);
    }
    if (typeof it.what !== 'string' || it.what.length > 160 || !/^\S.*\.$/.test(it.what) || /[.!?] /.test(it.what)) {
      bad(`${at}: "what" is one sentence of at most 160 characters that ends with a full stop`);
    }
    if (!STATUSES.includes(it.status)) return bad(`${at}: "status" is one of ${STATUSES.join(', ')}`);

    if (it.status !== 'shipped') {
      if ('version' in it || 'date' in it) bad(`${at}: only a shipped item has a version and a date`);
      return;
    }
    if (typeof it.date !== 'string' || !/^\d{4}-\d{2}-\d{2}$/.test(it.date) || Number.isNaN(Date.parse(it.date))) {
      return bad(`${at}: a shipped item has a "date" as YYYY-MM-DD`);
    }
    if (it.date > today) bad(`${at}: the date ${it.date} is in the future`);
    if (it.version === 'unreleased') {
      if (!hasUnreleased) bad(`${at}: CHANGELOG.md has no "## Unreleased" section`);
      if (it.date < lastRelease) bad(`${at}: dated ${it.date}, before the last release of ${lastRelease}, so it shipped in that release`);
    } else if (typeof it.version === 'string' && releases.has(it.version)) {
      if (it.date > releases.get(it.version)) bad(`${at}: dated ${it.date}, after ${it.version} came out on ${releases.get(it.version)}`);
    } else {
      bad(`${at}: "version" is "unreleased" or a release with a heading in CHANGELOG.md, not ${JSON.stringify(it.version)}`);
    }
  });

  const building = doc.items.filter((it) => it && it.status === 'in progress').length;
  if (building > MAX_IN_PROGRESS) bad(`${building} items are in progress, at most ${MAX_IN_PROGRESS} are`);
}

const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');
// "A cold tier" reads "a cold tier" inside a sentence, a leading "OIDC" would stay.
const inline = (s) => (/^[A-Z](?![A-Z0-9])/.test(s) ? s[0].toLowerCase() + s.slice(1) : s);
// "a, b and c", or "a; b; c" when an item has an "and" of its own ("backup and restore").
const list = (xs) => {
  if (xs.length < 2) return xs.join('');
  if (xs.some((x) => / and /.test(x))) return xs.join('; ');
  return `${xs.slice(0, -1).join(', ')} and ${xs[xs.length - 1]}`;
};
const day = (d) => {
  const [y, m, dd] = d.split('-').map(Number);
  return { short: `${MONTHS[m - 1]} ${dd}`, long: `${MONTHS[m - 1]} ${dd}, ${y}` };
};

// A part of a release shows no date of its own when it is the release's date.
function row(it, i, released) {
  const date = it.date && it.date !== released ? `<time datetime="${it.date}">${day(it.date).short}</time>` : '';
  return `${i}<li id="${it.id}"><h3>${esc(it.title)}</h3><p>${esc(it.what)}</p>${date}</li>`;
}

function stage(cls, title, note, items, i, released) {
  if (items.length === 0) return '';
  const n = note ? `\n${i}    <p>${note}</p>` : '';
  return `${i}<section class="stage ${cls}" aria-labelledby="st-${cls}">
${i}  <div class="stage-h">
${i}    <h2 id="st-${cls}">${title}</h2>${n}
${i}  </div>
${i}  <ol class="items">
${items.map((it) => row(it, `${i}    `, released)).join('\n')}
${i}  </ol>
${i}</section>`;
}

function renderPage(doc, changelog) {
  const by = (s) => doc.items.filter((it) => it.status === s);
  const shipped = by('shipped').sort((a, b) => (a.date < b.date ? 1 : a.date > b.date ? -1 : 0));
  const versions = [...new Set(shipped.map((it) => it.version))];
  const i = '    ';
  const parts = [
    stage('building', 'Being built', '', by('in progress'), i),
    stage('next', 'Next', 'In about this order.', by('next'), i),
    stage('later', 'Later', '', by('later'), i),
  ];
  for (const v of versions) {
    const items = shipped.filter((it) => it.version === v);
    if (v === 'unreleased') {
      parts.push(stage('unreleased', 'On main', 'Shipped since the last release, in the next one.', items, i));
    } else {
      const rel = changelog.match(new RegExp(`^## ${v.replace(/\./g, '\\.')} \\((\\d{4}-\\d{2}-\\d{2})\\)$`, 'm'))[1];
      const cls = `v${v.replace(/\./g, '-')}`;
      parts.push(stage(cls, v, `Released ${day(rel).long}.`, items, i, rel));
    }
  }
  return `\n${parts.filter(Boolean).join('\n')}\n    `;
}

function renderNow(doc) {
  const titles = (s) => doc.items.filter((it) => it.status === s).map((it) => esc(inline(it.title)));
  const now = titles('in progress');
  const next = titles('next');
  const say = [];
  if (now.length) say.push(`Being built: ${list(now)}.`);
  if (next.length) say.push(`Next: ${list(next)}.`);
  say.push('<a href="roadmap.html">The roadmap</a> has the rest, with dates.');
  return `<p class="open-next">${say.join(' ')}</p>`;
}

function put(html, name, body, file) {
  const re = new RegExp(`(<!--${name}:start-->)[\\s\\S]*?(<!--${name}:end-->)`);
  if (!re.test(html)) throw new Error(`no <!--${name}:start--> marker in ${file}`);
  return html.replace(re, (_, a, b) => a + body + b);
}

const doc = (() => {
  try {
    return JSON.parse(read(path.join(root, 'roadmap.json')));
  } catch (e) {
    console.error(`roadmap: roadmap.json does not parse: ${e.message}`);
    process.exit(1);
  }
})();
const changelog = read(path.join(root, 'CHANGELOG.md'));
validate(doc, changelog);
if (errors.length) {
  for (const e of errors) console.error(`roadmap: ${e}`);
  process.exit(1);
}

const pages = [
  ['roadmap.html', 'roadmap', renderPage(doc, changelog)],
  ['index.html', 'roadmap-now', renderNow(doc)],
];
const check = process.argv.includes('--check');
let stale = 0;
for (const [name, marker, body] of pages) {
  const file = path.join(site, name);
  const before = read(file);
  const after = put(before, marker, body, name);
  if (after === before) continue;
  if (check) {
    console.error(`roadmap: site/${name} differs from roadmap.json, run node site/tools/roadmap.cjs`);
    stale = 1;
  } else {
    fs.writeFileSync(file, after);
    console.log(`roadmap: wrote site/${name}`);
  }
}
process.exit(stale);
