// A stacked area of sizes per day, one band per series, drawn as SVG at the
// pixel width it gets. The pointer or the arrow keys pick a day, and the
// line under the chart reads each band's size on it, out loud too. The last
// day may still be counting: it is drawn lighter and dashed, never as a drop.

import { useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import { bytes, shown } from "../lib/text";
import { useWidth } from "./Histogram";

/**
 * One band: `color` is a CSS color, the same for the entity in every view.
 * `parts` are a few of what a summed band holds, read out with it.
 */
export type Series = { key: string; name: string; color: string; values: number[]; parts?: { name: string; values: number[] }[] };

const HEIGHT = 220;
const TOP = 10;
const BOTTOM = 22;
const LEFT = 64;
const RIGHT = 12;
/** The room the last label, `today, so far`, keeps from the one before it. */
const LAST_ROOM = 100;
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const ALONE = "Pick a project to see it alone.";

/** `Nov 20` for the UTC day `2026-11-20`. */
export function dayLabel(iso: string): string {
  const [, m, d] = iso.split("-").map(Number);
  return `${MONTHS[(m ?? 1) - 1] ?? ""} ${d ?? ""}`;
}

/** Up to `most` round steps of 1, 2 or 5 times a power of 10, from 0 to past `max`. */
function ticks(max: number, most = 4): number[] {
  if (max <= 0) return [0];
  const rough = max / most;
  const power = 10 ** Math.floor(Math.log10(rough));
  const step = ([1, 2, 5, 10].find((k) => k * power >= rough) ?? 10) * power;
  const out: number[] = [];
  for (let t = 0; t < max + step; t += step) out.push(t);
  return out;
}

/** Each series' running total per day, the first series at the bottom. */
function stack(series: Series[], n: number): number[][] {
  const below = new Array<number>(n).fill(0);
  return series.map((s) => {
    const top = below.map((b, i) => b + (s.values[i] ?? 0));
    top.forEach((t, i) => (below[i] = t));
    return top;
  });
}

/** The scales and paths of a chart `width` pixels wide. */
function geometry(n: number, series: Series[], width: number) {
  const tops = stack(series, n);
  const totals = tops.at(-1) ?? new Array<number>(n).fill(0);
  const grid = ticks(Math.max(...totals, 0));
  const max = grid.at(-1) || 1;
  const plotW = Math.max(1, width - LEFT - RIGHT);
  const plotH = HEIGHT - TOP - BOTTOM;
  const x = (i: number) => LEFT + (n > 1 ? (i * plotW) / (n - 1) : plotW / 2);
  const y = (v: number) => TOP + plotH - (v / max) * plotH;
  const point = (i: number, v: number) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`;
  /** The top of a band from day `from` to day `to`. */
  const line = (top: number[], from: number, to: number) =>
    top
      .slice(from, to + 1)
      .map((v, k) => `${k ? "L" : "M"}${point(from + k, v)}`)
      .join("");
  /** A band from day `from` to day `to`, down to the band below or the base. */
  const area = (top: number[], below: number[] | undefined, from: number, to: number) => {
    const back = [];
    for (let i = to; i >= from; i--) back.push(`L${point(i, below?.[i] ?? 0)}`);
    return `${line(top, from, to)}${back.join("")}Z`;
  };
  return { tops, totals, grid, plotW, plotH, x, y, line, area };
}

type Geometry = ReturnType<typeof geometry>;

type Props = {
  days: string[];
  series: Series[];
  /** What the chart shows, for screen readers, such as `Stored bytes per day`. */
  label: string;
  /** Whether the last day is today, still counting. */
  partial: boolean;
};

export function StackedArea({ days, series, label, partial }: Props) {
  const [box, width] = useWidth();
  const svg = useRef<SVGSVGElement>(null);
  const [at, setAt] = useState<number | null>(null);
  const n = days.length;
  const g = geometry(n, series, width);
  const index = (clientX: number) => {
    const left = svg.current?.getBoundingClientRect().left ?? 0;
    const step = n > 1 ? g.plotW / (n - 1) : g.plotW;
    return Math.max(0, Math.min(n - 1, Math.round((clientX - left - LEFT) / step)));
  };
  const onKey = (e: KeyboardEvent) => {
    const from = at ?? n - 1;
    const to = { ArrowLeft: from - 1, ArrowRight: from + 1, Home: 0, End: n - 1 }[e.key];
    if (to === undefined) return;
    e.preventDefault();
    setAt(Math.max(0, Math.min(n - 1, to)));
  };
  return (
    <>
      <div className="stack" ref={box}>
        {width > 0 ? (
          <svg
            ref={svg}
            width={width}
            height={HEIGHT}
            role="group"
            tabIndex={0}
            aria-label={`${summary(label, days, g.totals, partial)}. The left and right arrow keys pick a day, Home and End the first and the last.`}
            onPointerMove={(e: PointerEvent<SVGSVGElement>) => setAt(index(e.clientX))}
            onPointerLeave={(e) => {
              if (!e.currentTarget.matches(":focus-visible")) setAt(null);
            }}
            onKeyDown={onKey}
            onFocus={(e) => {
              if (e.currentTarget.matches(":focus-visible")) setAt((a) => a ?? n - 1);
            }}
            onBlur={() => setAt(null)}
          >
            <Plot g={g} days={days} series={series} partial={partial} at={at} />
          </svg>
        ) : null}
      </div>
      <Readout days={days} series={series} at={at} partial={partial} />
    </>
  );
}

/** What the chart holds in one sentence: the days, the sum and the biggest day. */
function summary(label: string, days: string[], totals: number[], partial: boolean): string {
  const total = totals.reduce((a, b) => a + b, 0);
  const peak = totals.indexOf(Math.max(...totals));
  const most = total > 0 && days[peak] ? `, most ${bytes(totals[peak] ?? 0)} on ${dayLabel(days[peak])}` : "";
  return `${label}, ${dayLabel(days[0] ?? "")} to ${dayLabel(days.at(-1) ?? "")} UTC${partial ? ", today so far" : ""}: ${bytes(total)} in all${most}`;
}

/** The grid, the bands, the day labels and the crosshair. */
function Plot({ g, days, series, partial, at }: { g: Geometry; days: string[]; series: Series[]; partial: boolean; at: number | null }) {
  const n = days.length;
  // Today, still counting, is the last segment on its own, drawn only once
  // something is in it, so the chart never shows today's start as a drop.
  const split = partial && n > 1;
  const solid = split ? n - 2 : n - 1;
  const today = split && (g.totals[n - 1] ?? 0) > 0;
  return (
    <>
      {g.grid.map((t) => (
        <g key={t} aria-hidden="true">
          <rect className={t === 0 ? "base" : "gl"} x={LEFT} y={Math.round(g.y(t))} width={g.plotW} height={1} rx={0.5} />
          <text x={LEFT - 8} y={g.y(t) + 4} textAnchor="end">
            {bytes(t)}
          </text>
        </g>
      ))}
      {g.tops.map((top, s) => (
        <g key={series[s]?.key} aria-hidden="true">
          <path className="band" d={g.area(top, g.tops[s - 1], 0, solid)} fill={series[s]?.color} />
          {today ? <path className="band part" d={g.area(top, g.tops[s - 1], n - 2, n - 1)} fill={series[s]?.color} /> : null}
        </g>
      ))}
      {g.tops.map((top, s) => (
        <g key={series[s]?.key} aria-hidden="true">
          <path className="gap" d={g.line(top, 0, solid)} />
          <path className="edge" d={g.line(top, 0, solid)} stroke={series[s]?.color} />
          {today ? <path className="edge part" d={g.line(top, n - 2, n - 1)} stroke={series[s]?.color} /> : null}
        </g>
      ))}
      <DayLabels g={g} days={days} partial={partial} />
      {at !== null ? (
        <g aria-hidden="true">
          <rect className="cross" x={g.x(at) - 0.5} y={TOP} width={1} height={g.plotH} rx={0.5} />
          {g.tops.map((top, s) =>
            (series[s]?.values[at] ?? 0) > 0 ? <circle key={series[s]?.key} className="dot" cx={g.x(at)} cy={g.y(top[at] ?? 0)} r={4} fill={series[s]?.color} /> : null,
          )}
        </g>
      ) : null}
    </>
  );
}

/** A label every so many days, counted back from the last, which says `today, so far` while it counts. */
function DayLabels({ g, days, partial }: { g: Geometry; days: string[]; partial: boolean }) {
  const n = days.length;
  const every = Math.max(1, Math.ceil(n / Math.max(1, Math.floor(g.plotW / 72))));
  return (
    <g aria-hidden="true">
      {days.map((d, i) => {
        const last = i === n - 1;
        if ((n - 1 - i) % every !== 0 || (partial && !last && g.x(n - 1) - g.x(i) < LAST_ROOM)) return null;
        return (
          <text key={d} className={partial && last ? "today" : undefined} x={g.x(i)} y={HEIGHT - 5} textAnchor={last ? "end" : i === 0 ? "start" : "middle"}>
            {partial && last ? "today, so far" : dayLabel(d)}
          </text>
        );
      })}
    </g>
  );
}

/**
 * The sizes of the picked day, under the chart, in the order of the legend.
 * It is a live region, so a screen reader says each day the arrow keys pick.
 */
function Readout({ days, series, at, partial }: { days: string[]; series: Series[]; at: number | null; partial: boolean }) {
  if (at === null) {
    return (
      <p className="sread idle" aria-live="polite" aria-atomic="true">
        Point at a day, or focus the chart and use the arrow keys, to read it here.
      </p>
    );
  }
  const today = partial && at === days.length - 1;
  const total = series.reduce((sum, s) => sum + (s.values[at] ?? 0), 0);
  const on = series.filter((s) => (s.values[at] ?? 0) > 0);
  const summed = on.find((s) => s.parts);
  return (
    <p className="sread" aria-live="polite" aria-atomic="true">
      <span className="sday">
        {dayLabel(days[at] ?? "")} UTC{today ? ", today so far" : ""}: <b>{bytes(total)}</b> stored{on.length > 0 ? ":" : "."}
      </span>
      {on.map((s, k) => (
        <span key={s.key} className="srow">
          <i style={{ background: s.color }} aria-hidden="true" />
          {shown(s.name)} {bytes(s.values[at] ?? 0)}
          {k < on.length - 1 ? "," : "."}
        </span>
      ))}
      {summed?.parts ? <Parts parts={summed.parts} at={at} /> : null}
    </p>
  );
}

/** The biggest projects of Other on one day, and how to see one alone. */
function Parts({ parts, at }: { parts: NonNullable<Series["parts"]>; at: number }) {
  const on = parts.filter((p) => (p.values[at] ?? 0) > 0);
  const most = on.map((p) => `${shown(p.name)} ${bytes(p.values[at] ?? 0)}`).join(", ");
  return <span className="sparts">{on.length > 0 ? `Most of Other: ${most}. ${ALONE}` : ALONE}</span>;
}

export function StackLegend({ series }: { series: Series[] }) {
  return (
    <ul className="slegend" aria-label="Projects in the chart">
      {series.map((s) => (
        <li key={s.key}>
          <i style={{ background: s.color }} aria-hidden="true" />
          {shown(s.name)}
        </li>
      ))}
    </ul>
  );
}

/** The columns of the table: each series, and after a summed one its biggest parts. */
function columns(series: Series[]): { key: string; name: string; values: number[] }[] {
  return series.flatMap((s) => [
    { key: s.key, name: shown(s.name), values: s.values },
    ...(s.parts ?? []).map((p, k) => ({ key: `${s.key}/${k}`, name: `${shown(p.name)}, in Other`, values: p.values })),
  ]);
}

/** The chart as a table: a row per day, newest first, a column per series. */
export function StackTable({ days, series, caption, partial }: { days: string[]; series: Series[]; caption: string; partial: boolean }) {
  const rows = days.map((d, i) => [d, i] as const).reverse();
  const cols = columns(series);
  const summed = series.some((s) => s.parts);
  return (
    <>
      <div className="tscroll sbox" role="region" aria-label={caption} tabIndex={0}>
        <table className="iss">
          <caption className="sr-only">{caption}</caption>
          <thead>
            <tr>
              <th scope="col" className="lead w-day">
                Day, UTC
              </th>
              {cols.map((c) => (
                <th key={c.key} scope="col" className="r">
                  {c.name}
                </th>
              ))}
              {series.length > 1 ? (
                <th scope="col" className="r">
                  Total
                </th>
              ) : null}
            </tr>
          </thead>
          <tbody>
            {rows.map(([d, i]) => (
              <tr key={d}>
                <th scope="row" className="lead">
                  {dayLabel(d)}
                  {partial && i === days.length - 1 ? <span className="usub">, so far</span> : null}
                </th>
                {cols.map((c) => (
                  <td key={c.key} className="r">
                    {bytes(c.values[i] ?? 0)}
                  </td>
                ))}
                {series.length > 1 ? <td className="r">{bytes(series.reduce((sum, s) => sum + (s.values[i] ?? 0), 0))}</td> : null}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {summed ? <p className="hint">The columns after Other are its biggest projects, already counted in it. {ALONE}</p> : null}
    </>
  );
}
