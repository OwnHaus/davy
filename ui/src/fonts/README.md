# Fonts

Three woff2 files that the binary serves itself, never from a CDN. They are
subset to Basic Latin, Latin-1 and Latin Extended-A (Czech names need it), plus
the quotes, arrows and signs the UI uses. Every licence notice is in the font's
name table, and the full text is in the `LICENSE-*.txt` files next to it.

| File | Source | Licence | What was kept |
| --- | --- | --- | --- |
| `display.woff2` | Hubot Sans variable, github.com/github/hubot-sans, `fonts/variable/HubotSansVF-Regular.ttf` | SIL OFL 1.1, `LICENSE-hubot-sans.txt` | width 108, weight 600 to 700, upright |
| `text.woff2` | Mona Sans 2.0.27 variable, github.com/github/mona-sans | SIL OFL 1.1, `LICENSE-mona-sans.txt` | width 100, optical size 16, weight 400 to 650, upright |
| `mono.woff2` | Commit Mono 1.143, github.com/eigilnikolajsen/commit-mono | SIL OFL 1.1, `LICENSE-commit-mono.txt` | regular 400 |

Hubot and Mona are Reserved Font Names. A subset is a Modified Version under the
OFL, so the subsets carry other family names in their name tables, `Davy
Display` and `Davy Text`. Commit Mono has no Reserved Font Name, and its subset
is called `Davy Mono` only so the three read alike in the CSS.

Built with fonttools:

```sh
fonttools varLib.instancer HubotSansVF-Regular.ttf wdth=108 wght=600:700 ital=0 -o display.ttf
fonttools varLib.instancer "MonaSansVF[wdth,opsz,wght].ttf" wdth=100 opsz=16 wght=400:650 -o text.ttf
pyftsubset <font> --flavor=woff2 --layout-features='kern,liga,tnum,lnum' \
  --unicodes='U+0020-007E,U+00A0-017F,U+2018-201E,U+2022,U+2026,U+2030-203A,U+2190-2193,U+2197,U+20AC,U+2122,U+2212,U+00D7'
```

Then the name table entries 1, 3, 4, 6, 16 and 17 were renamed, and entries 0,
13 and 14 of `display.woff2` were set to the copyright line and the OFL notice.
`scripts/ui-pack.mjs` fails the build when the three files together pass 100 KB.
