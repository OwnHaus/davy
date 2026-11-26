// What each project stored per UTC day: a stacked area, or the same numbers
// as a table. The first eight projects, oldest first, keep a hue each, the
// same one when picked alone. The rest share Other, in gray.

import { useState } from "react";
import type { UsageDays } from "../api/types";
import { plural, shown } from "../lib/text";
import { StackLegend, StackTable, StackedArea, type Series } from "./StackedArea";

/** Hues for the first eight projects, oldest first. Later ones share Other. */
const HUES = 8;
const hue = (i: number) => (i >= 0 && i < HUES ? `var(--s${i + 1})` : "var(--s0)");
const some = (values: number[]) => values.some((v) => v > 0);

/**
 * The bands of the chart, in the order of the rail: each project, then
 * Other. A project's key starts `p:` and Other's is empty, so no slug can
 * take Other's place. A band that stored nothing in the window is left out.
 */
export function bands(d: UsageDays): Series[] {
  const out: Series[] = d.projects.map((p) => ({ key: `p:${p.project}`, name: p.project, color: hue(d.names.indexOf(p.project)), values: p.stored }));
  const o = d.other;
  if (o) {
    const parts = o.top.map((t) => ({ name: t.project, values: t.stored }));
    out.push({ key: "", name: `Other, ${o.projects} ${plural(o.projects, "project")}`, color: hue(HUES), values: o.stored, parts });
  }
  return out.filter((s) => some(s.values));
}

type Props = { data: UsageDays; project: string | undefined; wide: boolean; onDays: (n: "90" | undefined) => void };

export function Stored({ data, project, wide, onDays }: Props) {
  const [table, setTable] = useState(false);
  const series = bands(data);
  const caption = `Stored bytes per day${project ? ` of ${shown(project)}` : " by project"}, UTC`;
  return (
    <>
      <div className="chead">
        {series.length > 1 ? <StackLegend series={series} /> : <span />}
        <div className="ctl">
          <div className="seg" role="group" aria-label="Days shown">
            <button type="button" aria-pressed={!wide} onClick={() => onDays(undefined)} data-text="30 days">
              30 days
            </button>
            <button type="button" aria-pressed={wide} onClick={() => onDays("90")} data-text="90 days">
              90 days
            </button>
          </div>
          {series.length > 0 ? (
            <button type="button" className="btn" aria-pressed={table} onClick={() => setTable((t) => !t)}>
              {table ? "Show chart" : "Show table"}
            </button>
          ) : null}
        </div>
      </div>
      {data.names.length === 0 ? (
        <p className="none">No projects yet.</p>
      ) : series.length === 0 ? (
        <p className="none">Nothing stored in these days.</p>
      ) : table ? (
        <StackTable days={data.days} series={series} caption={caption} partial />
      ) : (
        <StackedArea days={data.days} series={series} label={caption} partial />
      )}
    </>
  );
}
