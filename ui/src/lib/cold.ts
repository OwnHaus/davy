// Cold days are read for 5 s at most, and a block that fails its check is
// left out. A page that read cold logs says what it missed.

import type { ColdCut } from "../api/types";
import { plural } from "./text";
import { msOf, stamp } from "./time";

/** What `cuts` left out of cold logs, the latest stop and every failed check together; null when nothing was. */
export function joinCuts(cuts: (ColdCut | null)[]): ColdCut | null {
  const some = cuts.filter((c): c is ColdCut => c !== null);
  if (some.length === 0) return null;
  return {
    unscanned_to: some.findLast((c) => c.unscanned_to !== null)?.unscanned_to ?? null,
    corrupt_rows: some.reduce((n, c) => n + c.corrupt_rows, 0),
    corrupt_blocks: some.reduce((n, c) => n + c.corrupt_blocks, 0),
    corrupt_files: some.reduce((n, c) => n + c.corrupt_files, 0),
  };
}

/** `cut` in words, `read` naming what stopped, such as "The search"; null when nothing was left out. */
export function cutText(cut: ColdCut | null | undefined, read: string): string | null {
  if (!cut) return null;
  const parts: string[] = [];
  if (cut.unscanned_to !== null) {
    const to = stamp(new Date(msOf(cut.unscanned_to)).toISOString());
    parts.push(`${read} stopped after 5 s in cold logs, so logs up to ${to} may be missing. Narrow the range to read them.`);
  }
  if (cut.corrupt_blocks + cut.corrupt_files > 0) {
    const what = [
      cut.corrupt_blocks > 0 ? `${cut.corrupt_blocks} ${plural(cut.corrupt_blocks, "block")}` : null,
      cut.corrupt_files > 0 ? `${cut.corrupt_files} ${plural(cut.corrupt_files, "file")}` : null,
    ];
    parts.push(`Cold logs of ${what.filter((w) => w !== null).join(" and ")} were left out, as they failed a check. davy doctor names them.`);
  }
  return parts.length === 0 ? null : parts.join(" ");
}
