// A monitor's last 24 hours: one bar per hour. A bar's height is the mean
// response of the hour's checks. The top of the bar is red for the share of
// the hour's checks that failed. An hour an incident
// was open in is a full hatched bar, so it reads without color, and an hour
// with no check is a stub on the baseline.

import { useRef, useState, type KeyboardEvent } from "react";
import type { Hour } from "../api/types";
import { plural } from "../lib/text";
import { HOUR, hourOfDay, hourRange } from "../lib/time";
import { hideReadout, hot, readout } from "./HeatStrip";
import { useWidth } from "./Histogram";

const GAP = 2;
const SM_WIDTH = 166;
const HEIGHT = { sm: 16, lg: 72 };

/** A bar anchored to the baseline with its top corners rounded. */
function bar(x: number, w: number, h: number, base: number): string {
  const r = Math.min(2, w / 2, h);
  const y = base - h;
  return `M${x},${base}V${y + r}Q${x},${y} ${x + r},${y}H${x + w - r}Q${x + w},${y} ${x + w},${y + r}V${base}Z`;
}

const sum = (hourly: Hour[], of: (h: Hour) => number) => hourly.reduce((a, h) => a + of(h), 0);

/** `slowest 14:00 UTC at 151 ms mean`, or nothing without a timed check. */
export function slowestWords(hourly: Hour[], from: string): string | null {
  let at = -1;
  hourly.forEach((h, i) => {
    if (h.ms !== null && (at < 0 || h.ms > (hourly[at]?.ms ?? 0))) at = i;
  });
  const slow = hourly[at];
  if (!slow) return null;
  return `slowest ${hourOfDay(Date.parse(from) + at * HOUR)}:00 UTC at ${slow.ms} ms mean`;
}

export function responseSummary(hourly: Hour[], from: string): string {
  const checks = sum(hourly, (h) => h.checks);
  if (checks === 0) return "No checks in the last 24 hours";
  const failed = sum(hourly, (h) => h.failed);
  const slow = slowestWords(hourly, from);
  return `${checks} ${plural(checks, "check")} in the last 24 hours, ${failed} failed${slow ? `, ${slow}` : ""}`;
}

/** ` failed, mean 151 ms, incident open`, after `1 of 60 checks` in the readout. */
function also(h: Hour): string {
  return `${h.failed ? " failed" : ""}${h.ms === null ? "" : `, mean ${h.ms} ms`}${h.down ? ", incident open" : ""}`;
}

/** The hour's bar: hatched when down, else mean response with the failed share red at its top. */
function Mark({ h, x, w, height, max }: { h: Hour; x: number; w: number; height: number; max: number }) {
  if (h.down) return <path className="u-fail" d={bar(x, w, height, height)} fill="url(#hatch)" />;
  if (h.checks === 0) return <rect className="u-none" x={x} y={height - 2} width={w} height={2} rx={1} />;
  const all = h.ms === null ? height / 2 : Math.max(3, (h.ms / max) * height);
  if (h.failed === 0) return <path className="u-ok" d={bar(x, w, all, height)} />;
  let part = Math.min(all, Math.max(2, Math.round((h.failed / h.checks) * all)));
  const ok = all - part - 1;
  if (ok < 1) part = all;
  return (
    <>
      {ok >= 1 ? <rect className="u-ok" x={x} y={height - ok} width={w} height={ok} /> : null}
      <path className="u-part" d={bar(x, w, part, height - all + part)} />
    </>
  );
}

export function ResponseStrip({ hourly, from, size }: { hourly: Hour[]; from: string; size: "sm" | "lg" }) {
  const [box, measured] = useWidth();
  const svg = useRef<SVGSVGElement>(null);
  const [at, setAt] = useState(hourly.length - 1);
  const width = size === "sm" ? SM_WIDTH : measured;
  const height = HEIGHT[size];
  const n = hourly.length;
  const bw = n > 0 ? (width - GAP * (n - 1)) / n : 0;
  const max = Math.max(1, ...hourly.map((h) => h.ms ?? 0));
  const start = Date.parse(from);

  const show = (i: number) => {
    const g = svg.current?.querySelectorAll<SVGGElement>("g.cell")[i];
    if (!g) return;
    hot(0, g);
    readout(g);
  };
  const onKey = (e: KeyboardEvent) => {
    const to = { ArrowLeft: at - 1, ArrowRight: at + 1, Home: 0, End: n - 1 }[e.key];
    if (to === undefined) return;
    e.preventDefault();
    const next = Math.max(0, Math.min(n - 1, to));
    setAt(next);
    show(next);
  };

  const cells = hourly.map((h, i) => {
    const x = i * (bw + GAP);
    return (
      <g
        key={i}
        className="cell"
        data-start={start + i * HOUR}
        data-n={h.checks}
        data-part={h.failed || undefined}
        data-unit="check"
        data-also={also(h)}
      >
        <rect className="hit" x={x - 1} y={0} width={bw + 2} height={height} />
        <Mark h={h} x={x} w={bw} height={height} max={max} />
      </g>
    );
  });

  const summary = responseSummary(hourly, from);
  return (
    <div className={`ustrip ${size}`} ref={box}>
      {width > 0 ? (
        size === "sm" ? (
          <svg width={width} height={height} role="img" aria-label={summary}>
            {cells}
          </svg>
        ) : (
          <svg
            ref={svg}
            width={width}
            height={height}
            role="group"
            tabIndex={0}
            aria-label={`${summary}. Arrow keys read each hour.`}
            onKeyDown={onKey}
            onFocus={(e) => {
              if (e.currentTarget.matches(":focus-visible")) show(at);
            }}
            onBlur={hideReadout}
          >
            {cells}
          </svg>
        )
      ) : null}
    </div>
  );
}

/** The chart as a table, for screen readers. */
export function ResponseTable({ hourly, from, label }: { hourly: Hour[]; from: string; label: string }) {
  const start = Date.parse(from);
  return (
    <table className="sr-only">
      <caption>{label}: checks per hour, last 24 hours, UTC</caption>
      <thead>
        <tr>
          <th scope="col">Hour starting</th>
          <th scope="col">Checks</th>
          <th scope="col">Failed</th>
          <th scope="col">Mean response</th>
          <th scope="col">Incident</th>
        </tr>
      </thead>
      <tbody>
        {hourly.map((h, i) => (
          <tr key={i}>
            <th scope="row">{hourRange(start + i * HOUR)}</th>
            <td>{h.checks}</td>
            <td>{h.failed}</td>
            <td>{h.ms === null ? "none" : `${h.ms} ms`}</td>
            <td>{h.down ? "open" : "none"}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export function UptimeLegend() {
  return (
    <div className="legend ulegend">
      <h2>Each hour</h2>
      <ul>
        <li>
          <svg width="10" height="14" aria-hidden="true">
            <path className="u-ok" d={bar(0, 10, 9, 14)} />
          </svg>
          Height is mean response
        </li>
        <li>
          <svg width="10" height="14" aria-hidden="true">
            <rect className="u-ok" x={0} y={8} width={10} height={6} />
            <path className="u-part" d={bar(0, 10, 4, 7)} />
          </svg>
          Red top is the failed share
        </li>
        <li>
          <svg width="10" height="14" aria-hidden="true">
            <path className="u-fail" d={bar(0, 10, 14, 14)} fill="url(#hatch)" />
          </svg>
          An incident was open
        </li>
        <li>
          <svg width="10" height="14" aria-hidden="true">
            <rect className="u-none" x={0} y={12} width={10} height={2} rx={1} />
          </svg>
          No checks
        </li>
      </ul>
    </div>
  );
}

/** The hatch a down hour is filled with. Render once, in the shell. */
export function Hatch() {
  return (
    <svg className="defs" width="0" height="0" aria-hidden="true">
      <defs>
        <pattern id="hatch" width="4" height="4" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
          <rect className="hb" width="4" height="4" />
          <rect className="hs" width="2" height="4" />
        </pattern>
      </defs>
    </svg>
  );
}
