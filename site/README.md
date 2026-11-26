# The Davy website

A static site with no build step and no external requests: `index.html`, `roadmap.html`, `style.css` and `app.js`.
The headings use Bagnard (SIL Open Font License, in `fonts/`), inlined into `style.css`.
`vercel.json` sets the security headers when the page is served from Vercel.

Open `index.html` from disk to see it. Everything is in the HTML, so the page reads without JavaScript too.

## Tools

- `node tools/gen-svg.cjs` draws the lamp, the cutaway and the header mark into `index.html`. The small lamps in the story and the simulator are written by hand.
- `node tools/roadmap.cjs` checks `../roadmap.json` and writes it into `roadmap.html` and the line under "What 0.1.0 does not do yet" in `index.html`. `--check` fails when either is behind; `scripts/check` runs it.
- `node tools/shoot.cjs` renders both pages into `shots/` at 1440 and 390 px in light and dark, checks for horizontal overflow and overlapping header links at 1440, 390 and 360 px, and lists any external request. It uses Playwright from `e2e/`, so run `pnpm install` there first.

## Keep in step with Davy

- The simulator follows the reopen rule in `IssueStatus::on_event` in `src/store.rs`. Change both together.
- The binary sizes come from the 0.1.0 release. Update them with the next release.
- The roadmap comes from `roadmap.json`. Edit it there, never in the HTML.
- The screenshots in `img/` come from the demo seed in `e2e/demo/seed.mjs`. The copy names issue 1 from that seed.
- On phones the screenshots are crops set by hand with `--fx`, `--fy` and `--z` on each image.
