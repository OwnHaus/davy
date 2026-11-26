// Text from an event is untrusted. It renders only as text, and characters
// that would hide or reorder what a person reads show as escapes.

// C0 and C1 controls except tab and newline, zero-width characters, line and
// paragraph separators, bidi marks, embeddings, overrides and isolates, and
// the byte order mark.
const RANGES: [number, number][] = [
  [0x00, 0x08], [0x0b, 0x1f], [0x7f, 0x9f], [0x061c, 0x061c], [0x200b, 0x200f],
  [0x2028, 0x202e], [0x2060, 0x2069], [0xfeff, 0xfeff],
];
const hex = (n: number) => n.toString(16).padStart(4, "0");
const HIDDEN = new RegExp(`[${RANGES.map(([a, b]) => `\\u${hex(a)}-\\u${hex(b)}`).join("")}]`, "g");

export function shown(text: string): string {
  return text.replace(HIDDEN, (c) => `\\u{${hex(c.charCodeAt(0))}}`);
}

/** The URL, when it is one a link may point to. */
export function webUrl(text: string): string | null {
  try {
    const url = new URL(text);
    return url.protocol === "http:" || url.protocol === "https:" ? url.href : null;
  } catch {
    return null;
  }
}

/** The word for `n` things, without the number: `plural(2, "issue")` is `issues`. */
export function plural(n: number, one: string, many = `${one}s`): string {
  return n === 1 ? one : many;
}

const UNITS = ["B", "KB", "MB", "GB", "TB", "PB"];

/** A size as `davy usage` prints it: `512 B`, `2.3 KB`, powers of 1000. */
export function bytes(n: number): string {
  let unit = 0;
  let scale = 1;
  while (unit + 1 < UNITS.length && n >= scale * 1000) {
    unit += 1;
    scale *= 1000;
  }
  if (unit === 0) return `${n} B`;
  return `${(Math.round((n * 10) / scale) / 10).toFixed(1)} ${UNITS[unit]}`;
}
