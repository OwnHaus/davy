// The log histogram: how many logs match in each round bucket of the range.
// Drawn as SVG at the pixel width it gets, so bars keep their rounded tops.
// Drag across it, or pick a bar, to zoom the range to those buckets.

import { useEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import type { LogHistogram } from "../api/types";
import { day, hms, msOf, sameDay, span } from "../lib/time";
import { hideReadout, hot, readout } from "./HeatStrip";

const HEIGHT = 84;
const PLOT = 62;
const TOP = 6;
/** Label every k-th bucket, k from this list, so labels sit on round times. */
const EVERY = [1, 2, 3, 4, 5, 6, 8, 10, 12, 15, 20, 24, 30, 40, 48, 60, 100, 120, 150, 200];
const DAY_NS = 86_400_000_000_000n;
const SECOND_NS = 1_000_000_000n;
const MINUTE_NS = 60n * SECOND_NS;

export type Bucket = { from: bigint; to: bigint; n: number };

export function buckets(h: LogHistogram): Bucket[] {
  const from = BigInt(h.from);
  const step = BigInt(h.step);
  return h.counts.map((n, i) => {
    const start = from + BigInt(i) * step;
    return { from: start, to: start + step - 1n, n };
  });
}

/** The words for a bucket's time, as precise as its step. */
function when(b: Bucket, step: bigint): string {
  const from = msOf(b.from);
  const to = msOf(b.to);
  if (step >= DAY_NS) return sameDay(from, to) ? day(from) : `${day(from)} to ${day(to)}`;
  return span(from, to, step < SECOND_NS);
}

function label(b: Bucket, step: bigint): string {
  const ms = msOf(b.from);
  if (step >= DAY_NS) return day(ms);
  if (step < SECOND_NS) return hms(ms);
  if (step < MINUTE_NS) return hms(ms, false);
  return b.from % DAY_NS === 0n ? day(ms) : hms(ms, false).slice(0, 5);
}

/** A bar anchored to the baseline with its top corners rounded. */
function bar(x: number, w: number, h: number): string {
  const r = Math.min(2, w / 2, h);
  const y = TOP + PLOT - h;
  return `M${x},${TOP + PLOT}V${y + r}Q${x},${y} ${x + r},${y}H${x + w - r}Q${x + w},${y} ${x + w},${y + r}V${TOP + PLOT}Z`;
}

/** A ref for a box and its width in whole pixels, kept up to date. */
export function useWidth() {
  const ref = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const seen = new ResizeObserver(([entry]) => setWidth(Math.floor(entry?.contentRect.width ?? 0)));
    seen.observe(el);
    return () => seen.disconnect();
  }, []);
  return [ref, width] as const;
}

type Props = {
  data: LogHistogram;
  /** Zooms the range to `from` and `to`, nanoseconds, both included. */
  onZoom: (from: string, to: string) => void;
};

export function Histogram({ data, onZoom }: Props) {
  const [box, width] = useWidth();
  const svg = useRef<SVGSVGElement>(null);
  const all = buckets(data);
  const step = BigInt(data.step);
  const n = all.length;
  const max = Math.max(1, ...data.counts);
  const total = data.counts.reduce((a, b) => a + b, 0);
  const bw = n > 0 ? width / n : 0;
  const gap = bw >= 6 ? 2 : bw >= 3 ? 1 : 0;
  const [drag, setDrag] = useState<[number, number] | null>(null);
  const [at, setAt] = useState(n - 1);

  const index = (clientX: number) => {
    const left = svg.current?.getBoundingClientRect().left ?? 0;
    return Math.max(0, Math.min(n - 1, Math.floor((clientX - left) / (bw || 1))));
  };
  const zoom = (a: number, b: number) => {
    const first = all[Math.min(a, b)];
    const last = all[Math.max(a, b)];
    if (!first || !last) return;
    hideReadout();
    onZoom(String(first.from), String(last.to));
  };
  const show = (i: number) => {
    const g = svg.current?.querySelectorAll<SVGGElement>("g.cell")[i];
    if (!g) return;
    hot(0, g);
    readout(g);
  };

  const down = (e: PointerEvent<SVGSVGElement>) => {
    if (e.button !== 0 || n === 0) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    const i = index(e.clientX);
    setDrag([i, i]);
  };
  const move = (e: PointerEvent<SVGSVGElement>) => {
    if (drag) setDrag([drag[0], index(e.clientX)]);
  };
  const up = (e: PointerEvent<SVGSVGElement>) => {
    if (!drag) return;
    setDrag(null);
    zoom(drag[0], index(e.clientX));
  };
  const onKey = (e: KeyboardEvent) => {
    const to = { ArrowLeft: at - 1, ArrowRight: at + 1, Home: 0, End: n - 1 }[e.key];
    if (e.key === "Enter") {
      zoom(at, at);
    } else if (to !== undefined) {
      const next = Math.max(0, Math.min(n - 1, to));
      setAt(next);
      show(next);
    } else {
      return;
    }
    e.preventDefault();
  };

  const peakAt = data.counts.indexOf(Math.max(...data.counts));
  const peak = all[peakAt];
  const summary =
    total === 0
      ? "No logs match in this range"
      : `${total.toLocaleString("en-US")} matching logs${peak ? `, most ${peak.n.toLocaleString("en-US")} in ${when(peak, step)}` : ""}`;

  const first = all[0] ? all[0].from / step : 0n;
  const every = EVERY.find((k) => k * bw >= 84) ?? n;
  const sel = drag ? [Math.min(...drag), Math.max(...drag)] : null;

  return (
    <div className="hist" ref={box}>
      {width > 0 ? (
        <svg
          ref={svg}
          width={width}
          height={HEIGHT}
          role="group"
          tabIndex={0}
          aria-label={`${summary}. Arrow keys read each bar, Enter zooms into it. Dragging across bars zooms to them.`}
          onPointerDown={down}
          onPointerMove={move}
          onPointerUp={up}
          onPointerCancel={() => setDrag(null)}
          onKeyDown={onKey}
          onFocus={(e) => {
            if (e.currentTarget.matches(":focus-visible")) show(at);
          }}
          onBlur={hideReadout}
        >
          {sel ? <rect className="zsel" x={(sel[0] ?? 0) * bw} y={0} width={((sel[1] ?? 0) - (sel[0] ?? 0) + 1) * bw} height={TOP + PLOT} rx={2} /> : null}
          <rect className="base" x={0} y={TOP + PLOT} width={width} height={1} rx={0.5} />
          {all.map((b, i) => {
            const h = b.n === 0 ? 0 : Math.max(2, (b.n / max) * PLOT);
            const x = i * bw;
            return (
              <g key={i} className="cell" data-n={b.n} data-unit="log" data-when={when(b, step)}>
                <rect className="hit" x={x} y={0} width={bw} height={TOP + PLOT} />
                {h > 0 ? <path className="bar" d={bar(x + gap / 2, Math.max(1, bw - gap), h)} /> : null}
              </g>
            );
          })}
          {all.map((b, i) =>
            (first + BigInt(i)) % BigInt(every) === 0n && i * bw > 40 && i * bw < width - 40 ? (
              <text key={i} x={i * bw} y={HEIGHT - 3} textAnchor="middle" aria-hidden="true">
                {label(b, step)}
              </text>
            ) : null,
          )}
        </svg>
      ) : null}
      <HistogramTable data={all} step={step} />
    </div>
  );
}

/** The histogram as a table, for screen readers. Empty buckets are left out. */
function HistogramTable({ data, step }: { data: Bucket[]; step: bigint }) {
  return (
    <table className="sr-only">
      <caption>Matching logs over time, UTC</caption>
      <thead>
        <tr>
          <th scope="col">Time</th>
          <th scope="col">Logs</th>
        </tr>
      </thead>
      <tbody>
        {data
          .filter((b) => b.n > 0)
          .map((b) => (
            <tr key={String(b.from)}>
              <th scope="row">{when(b, step)}</th>
              <td>{b.n}</td>
            </tr>
          ))}
      </tbody>
    </table>
  );
}
