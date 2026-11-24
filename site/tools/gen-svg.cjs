// Generates the engraved lamp and the gauze cutaway as inline SVG and writes
// them into index.html between marker comments. Run: node tools/gen-svg.cjs
const fs = require('fs');
const path = require('path');

const f = (n) => Math.round(n * 10) / 10;
const rad = (d) => (d * Math.PI) / 180;

// Front half of a horizontal circle seen slightly from above.
const arc = (cx, y, r, ry) => `M${f(cx - r)} ${f(y)}A${f(r)} ${f(ry)} 0 0 0 ${f(cx + r)} ${f(y)}`;

// Engraver's shading on a cylinder lit from the upper left: vertical strokes,
// none on the lit side, closer and heavier toward the right edge.
function cylHatch(cx, r, ry, y0, y1, n, from = -0.15) {
  const groups = [];
  for (let k = 0; k <= n; k++) {
    const u = k / n;
    const t = from + (Math.PI / 2 - 0.04 - from) * Math.pow(u, 0.62);
    const x = cx + r * Math.sin(t);
    const dy = ry * Math.cos(t);
    const w = 0.45 + 1.0 * u;
    groups.push(`<path d="M${f(x)} ${f(y0 + dy)}V${f(y1 + dy)}" stroke-width="${f(w * 10) / 10}"/>`);
  }
  return groups.join('');
}

function mesh(cx, r, ry, y0, y1, stepDeg, stepY, offDeg = 0, offY = 0) {
  let d = '';
  for (let a = -90 + offDeg; a <= 90; a += stepDeg) {
    const t = rad(a);
    const x = cx + r * Math.sin(t);
    const dy = ry * Math.cos(t);
    d += `M${f(x)} ${f(y0 + dy)}V${f(y1 + dy)}`;
  }
  for (let y = y0 + offY; y <= y1; y += stepY) d += arc(cx, y, r, ry);
  return d;
}

// Solid cylinder band: side silhouette plus the visible front edge at the bottom.
function band(cx, r, ry, y0, y1, cls = 'ln') {
  return `<path class="${cls}" d="M${f(cx - r)} ${f(y0)}V${f(y1)}A${f(r)} ${f(ry)} 0 0 0 ${f(cx + r)} ${f(y1)}V${f(y0)}"/>` +
    `<ellipse class="${cls} paper" cx="${cx}" cy="${y0}" rx="${r}" ry="${ry}"/>`;
}
function bandFill(cx, r, ry, y0, y1) {
  return `<path class="paper" d="M${f(cx - r)} ${f(y0)}V${f(y1)}A${f(r)} ${f(ry)} 0 0 0 ${f(cx + r)} ${f(y1)}V${f(y0)}A${f(r)} ${f(ry)} 0 0 1 ${f(cx - r)} ${f(y0)}Z"/>`;
}

function lamp() {
  const cx = 150;
  const g = { r: 36, ry: 7, y0: 104, y1: 520 };
  const parts = [];

  // ground hatching, lit from the upper left so the shadow falls right
  const ground = [];
  for (let i = 0; i < 7; i++) {
    const y = 702 + i * 5;
    const x0 = 112 + i * 6;
    const x1 = 236 - i * 9 + (i % 2) * 6;
    ground.push(`M${x0} ${y}H${x1}`);
  }
  parts.push(`<path class="hatch" stroke-width=".8" d="${ground.join('')}"/>`);

  // pricker wire and nut under the vessel
  parts.push(`<path class="ln" d="M150 692V724"/><rect class="ln paper" x="144" y="716" width="12" height="9" rx="1.5"/>`);

  // vessel (oil fount)
  parts.push(bandFill(cx, 64, 11, 676, 690) + band(cx, 64, 11, 676, 690));
  parts.push(`<g class="hatch">${cylHatch(cx, 64, 11, 676, 690, 16)}</g>`);
  parts.push(bandFill(cx, 60, 11, 552, 676) + band(cx, 60, 11, 552, 676));
  parts.push(`<g class="hatch">${cylHatch(cx, 60, 11, 556, 676, 30)}</g>`);
  // a raised filler boss on the vessel, engraved
  parts.push(`<path class="ln" d="M96 606h10v22h-10"/><path class="ln" d="M101 606v22" stroke-width=".6"/>`);

  // collar that screws the gauze onto the vessel, with thread lines
  parts.push(bandFill(cx, 50, 9, 518, 552) + band(cx, 50, 9, 518, 552));
  let threads = '';
  for (let y = 526; y <= 548; y += 3) threads += arc(cx, y, 50, 9);
  parts.push(`<path class="hatch" stroke-width=".55" d="${threads}"/>`);
  parts.push(`<g class="hatch">${cylHatch(cx, 50, 9, 518, 552, 14, 0.4)}</g>`);

  // back pillar, seen through the gauze
  parts.push(`<path class="ln faint" d="M150 96V520" stroke-width="3"/>`);

  // wick tube and wick
  parts.push(`<path class="ln paper" d="M144 506h12v14h-12z"/><path class="ln" d="M147.5 506v-4h5v4" stroke-width="1"/>`);

  // firedamp cap and flame (styled from CSS)
  parts.push(`<g class="cap"><path d="M150 286C171 352 186 426 186 468C186 494 171 507 150 507C129 507 114 494 114 468C114 426 129 352 150 286Z"/><path class="cap-edge" d="M150 286C171 352 186 426 186 468C186 494 171 507 150 507C129 507 114 494 114 468C114 426 129 352 150 286Z"/></g>`);
  parts.push(`<g class="flame"><g class="flame-in">` +
    `<path class="f-outer" d="M150 440C156 457 164 473 164 488C164 499 158 507 150 507C142 507 136 499 136 488C136 473 144 457 150 440Z"/>` +
    `<path class="f-core" d="M150 468C153.5 478 157 487 157 494C157 500.5 154 504 150 504C146 504 143 500.5 143 494C143 487 146.5 478 150 468Z"/>` +
    `<path class="f-blue" d="M143.5 500C145 504 147.5 506 150 506C152.5 506 155 504 156.5 500C154 502 146 502 143.5 500Z"/>` +
    `</g></g>`);

  // gauze: one wire mesh, doubled at the top where the heat collects
  const m1 = mesh(cx, g.r, g.ry, g.y0, g.y1, 6, 5.2);
  const m2 = mesh(cx, g.r, g.ry, g.y0, 196, 6, 5.2, 3, 2.6);
  parts.push(`<path id="mesh" class="mesh" d="${m1}"/>`);
  parts.push(`<path class="mesh" d="${m2}"/>`);
  // the flame lights the wire nearest to it
  parts.push(`<g mask="url(#lit)"><use href="#mesh" class="mesh-lit"/></g>`);
  parts.push(`<path class="ln" d="M${cx - g.r} ${g.y0}V${g.y1}M${cx + g.r} ${g.y0}V${g.y1}"/>`);
  parts.push(`<path class="ln" stroke-width=".8" d="${arc(cx, 196, g.r, g.ry)}"/>`);
  // shading on the gauze, right side
  parts.push(`<g class="hatch" opacity=".55">${cylHatch(cx, g.r, g.ry, 110, 516, 9, 0.9)}</g>`);

  // front pillars at plus and minus 60 degrees
  for (const s of [-1, 1]) {
    const x = cx + s * 46;
    parts.push(`<path class="pillar" d="M${x - 3} 100V522h6V100z"/>`);
    parts.push(`<path class="ln" stroke-width=".7" d="M${x + (s > 0 ? 1.4 : 1.4)} 104V518"/>`);
  }

  // top plate, boss, swivel and carrying ring
  parts.push(bandFill(cx, 52, 9, 90, 104) + band(cx, 52, 9, 90, 104));
  parts.push(`<g class="hatch">${cylHatch(cx, 52, 9, 90, 104, 12)}</g>`);
  parts.push(`<path class="ln paper" d="M136 90L142 70H158L164 90Z"/>`);
  parts.push(`<path class="hatch" stroke-width=".7" d="M153 73L156 88M156 73L159.5 88"/>`);
  parts.push(`<path class="ln paper" d="M146 70V58H154V70Z"/>`);
  parts.push(`<ellipse class="ring" cx="150" cy="34" rx="19" ry="24"/>`);
  parts.push(`<path class="ln" stroke-width=".7" d="M163 20C167 26 168 36 165 46"/>`);

  const defs = `<defs>
    <linearGradient id="litg" gradientUnits="userSpaceOnUse" x1="0" y1="300" x2="0" y2="560">
      <stop offset="0" stop-color="#fff" stop-opacity="0"/>
      <stop offset=".55" stop-color="#fff" stop-opacity=".9"/>
      <stop offset=".85" stop-color="#fff" stop-opacity=".6"/>
      <stop offset="1" stop-color="#fff" stop-opacity="0"/>
    </linearGradient>
    <mask id="lit" maskUnits="userSpaceOnUse" x="0" y="0" width="300" height="800"><rect class="lit-rect" x="100" y="300" width="100" height="260" fill="url(#litg)"/></mask>
  </defs>`;

  return `<svg class="lamp" viewBox="40 0 220 744" role="img" aria-labelledby="lamp-t">
  <title id="lamp-t">An engraving of a miner's safety lamp: a brass vessel, a wire gauze cylinder with a flame inside, and a ring to carry it.</title>${defs}
  ${parts.join('\n  ')}
</svg>`;
}

// Small mark for the nav and favicon: gauze, flame, ring.
function mark() {
  return `<svg class="mark" viewBox="0 0 20 32" aria-hidden="true">
    <ellipse cx="10" cy="3.6" rx="2.6" ry="2.8" fill="none" stroke="currentColor" stroke-width="1.2"/>
    <path d="M4.5 7.5h11v1.6h-11z" fill="currentColor"/>
    <path d="M6 9.1v14M8 9.1v14M10 9.1v14M12 9.1v14M14 9.1v14M6 12h8M6 15h8M6 18h8M6 21h8" stroke="currentColor" stroke-width=".55" opacity=".7"/>
    <path class="mark-cap" d="M10 10.6c2.5 3.6 3.7 7 3.7 9.6 0 1.8-1.6 2.9-3.7 2.9s-3.7-1.1-3.7-2.9c0-2.6 1.2-6 3.7-9.6z"/>
    <path class="mark-flame" d="M10 15.4c1 1.6 1.7 2.9 1.7 4.1 0 1.2-.8 2-1.7 2s-1.7-.8-1.7-2c0-1.2.7-2.5 1.7-4.1z"/>
    <path d="M4 23.1h12v2.4H4zM3.4 25.5h13.2v4.6c0 .8-.6 1.4-1.4 1.4H4.8c-.8 0-1.4-.6-1.4-1.4z" fill="currentColor"/>
  </svg>`;
}

// Cutaway of the gauze: the whole stack inside one binary.
function cutaway() {
  const cx = 260, r = 200, ry = 36, y0 = 56, y1 = 548;
  const parts = [];
  // back wall of the cylinder (inner arc), seen through the window
  parts.push(`<path class="ln faint" d="M${cx - r} ${y0}A${r} ${ry} 0 0 1 ${cx + r} ${y0}"/>`);
  // left and right mesh panels; the middle is cut away
  let d = '';
  for (let a = -90; a <= 90; a += 4.5) {
    if (Math.abs(a) < 48) continue;
    const t = rad(a);
    const x = cx + r * Math.sin(t);
    const dy = ry * Math.cos(t);
    d += `M${f(x)} ${f(y0 + dy)}V${f(y1 + dy)}`;
  }
  const cutL = cx + r * Math.sin(rad(-48));
  const cutR = cx + r * Math.sin(rad(48));
  for (let y = y0; y <= y1; y += 7) {
    // arc from the left edge to the cut, and from the cut to the right edge
    const dy = ry * Math.cos(rad(48));
    d += `M${f(cx - r)} ${f(y)}A${r} ${ry} 0 0 0 ${f(cutL)} ${f(y + dy)}`;
    d += `M${f(cutR)} ${f(y + dy)}A${r} ${ry} 0 0 0 ${f(cx + r)} ${f(y)}`;
  }
  parts.push(`<path class="mesh" d="${d}"/>`);
  parts.push(`<g class="hatch" opacity=".6">${cylHatch(cx, r, ry, y0 + 6, y1 - 4, 10, 0.9)}</g>`);

  // shelves inside, one per part of the binary
  const shelves = ['A', 'B', 'C', 'D', 'E', 'F'];
  const top = 118, gap = 74;
  const sw = 150; // half width of a shelf (inside the cut)
  shelves.forEach((L, i) => {
    const y = top + i * gap;
    parts.push(`<path class="paper ln" d="M${cx - sw} ${y}V${y + 12}A${sw} 22 0 0 0 ${cx + sw} ${y + 12}V${y}A${sw} 22 0 0 0 ${cx - sw} ${y}Z"/>`);
    parts.push(`<ellipse class="ln shelf-top" cx="${cx}" cy="${y}" rx="${sw}" ry="22"/>`);
    parts.push(`<g class="hatch">${cylHatch(cx, sw, 22, y + 1, y + 12, 9, 0.5)}</g>`);
    parts.push(`<text class="key" x="${cx}" y="${y + 1}" text-anchor="middle" dominant-baseline="central">${L}</text>`);
  });

  // cut edges, the rim and the silhouette
  const dyc = ry * Math.cos(rad(48));
  parts.push(`<path class="ln cut" d="M${f(cutL)} ${f(y0 + dyc)}V${f(y1 + dyc)}M${f(cutR)} ${f(y0 + dyc)}V${f(y1 + dyc)}"/>`);
  parts.push(`<path class="ln" d="M${cx - r} ${y0}V${y1}A${r} ${ry} 0 0 0 ${cx + r} ${y1}V${y0}"/>`);
  parts.push(`<path class="ln" d="M${cx - r} ${y0}A${r} ${ry} 0 0 0 ${f(cutL)} ${f(y0 + dyc)}M${f(cutR)} ${f(y0 + dyc)}A${r} ${ry} 0 0 0 ${cx + r} ${y0}"/>`);
  parts.push(`<path class="ln" d="M${cx - r} ${y1}A${r} ${ry} 0 0 0 ${cx + r} ${y1}" stroke-width="1.6"/>`);

  return `<svg class="cutaway" viewBox="40 30 440 570" role="img" aria-labelledby="cut-t">
  <title id="cut-t">A cutaway drawing of the lamp's gauze cylinder with six shelves inside, lettered A to F. The key beside it names each part.</title>
  ${parts.join('\n  ')}
</svg>`;
}

const file = path.join(__dirname, '..', 'index.html');
let html = fs.readFileSync(file, 'utf8');
const put = (name, svg) => {
  const re = new RegExp(`(<!--${name}:start-->)[\\s\\S]*?(<!--${name}:end-->)`, 'g');
  if (!re.test(html)) throw new Error('missing marker ' + name);
  html = html.replace(re, `$1${svg}$2`);
};
put('lamp', lamp());
put('cutaway', cutaway());
put('mark', mark());
fs.writeFileSync(file, html);
console.log('ok', Buffer.byteLength(html));
