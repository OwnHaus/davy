// The 24 hour heat strip. One scale for every row, so two rows compare at a
// glance. The readout is one fixed element for the whole page, fed by a
// delegated pointer listener, because the list draws 50 strips of 24 cells.

import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { HOUR, day, hourOfDay, hourRange } from "../lib/time";

// Each label is the least count of its step, so six fit under the swatches.
const LEGEND = ["0", "1", "3", "7", "15", "30+"];

export function step(n: number): number {
  return n <= 0 ? 0 : n < 3 ? 1 : n < 7 ? 2 : n < 15 ? 3 : n < 30 ? 4 : 5;
}

const total = (hourly: number[]) => hourly.reduce((a, b) => a + b, 0);

function peak(hourly: number[]): number {
  let at = 0;
  hourly.forEach((n, i) => {
    if (n > (hourly[at] ?? 0)) at = i;
  });
  return at;
}

/** `peak 4 at 08:00 UTC`, or nothing when the strip is empty. */
export function peakWords(hourly: number[], from: string): string | null {
  if (total(hourly) === 0) return null;
  const at = peak(hourly);
  return `peak ${hourly[at]} at ${hourOfDay(Date.parse(from) + at * HOUR)}:00 UTC`;
}

export function summary(hourly: number[], from: string): string {
  const n = total(hourly);
  const words = peakWords(hourly, from);
  return `Events per hour, last 24 hours: ${n} in total${words ? `, ${words}` : ""}`;
}

type Size = "sm" | "md" | "lg";

export function Strip({ hourly, from, size }: { hourly: number[]; from: string; size: Size }) {
  const start = Date.parse(from);
  const [at, setAt] = useState(hourly.length - 1);
  const ref = useRef<HTMLDivElement>(null);
  const scrub = size !== "sm";

  const go = (i: number) => {
    const next = Math.max(0, Math.min(hourly.length - 1, i));
    setAt(next);
    const cell = ref.current?.children[next];
    if (cell instanceof HTMLElement) {
      hot(-1);
      cell.classList.add("on");
      readout(cell);
    }
  };
  const onKey = (e: KeyboardEvent) => {
    const to = { ArrowLeft: at - 1, ArrowRight: at + 1, Home: 0, End: hourly.length - 1 }[e.key];
    if (to === undefined) return;
    e.preventDefault();
    go(to);
  };
  const cells = hourly.map((n, i) => (
    <i key={i} className={`cell s${step(n)}`} data-start={start + i * HOUR} data-n={n}>
      {size === "lg" && n > 0 ? n : null}
    </i>
  ));
  if (!scrub) {
    return (
      <div className={`strip ${size}`} role="img" aria-label={summary(hourly, from)}>
        {cells}
      </div>
    );
  }
  return (
    <div
      ref={ref}
      className={`strip ${size}`}
      role="group"
      tabIndex={0}
      aria-label={`${summary(hourly, from)}. Arrow keys read each hour.`}
      onKeyDown={onKey}
      onFocus={(e) => {
        if (e.currentTarget.matches(":focus-visible")) go(at);
      }}
      onBlur={() => {
        tip()?.classList.remove("show");
        hot(-1);
      }}
    >
      {cells}
    </div>
  );
}

/** Hour labels under a strip. The small one marks every sixth hour and now. */
export function Axis({ from, size }: { from: string; size: Size }) {
  const start = Date.parse(from);
  const spans = Array.from({ length: 24 }, (_, i) => {
    const at = start + i * HOUR;
    const hh = hourOfDay(at);
    if (size === "lg") {
      return hh === "00" ? (
        <span key={i} className="day">
          {hh}
          <br />
          {day(at)}
        </span>
      ) : (
        <span key={i}>{hh}</span>
      );
    }
    if (i === 23) {
      return (
        <span key={i} className="end">
          now
        </span>
      );
    }
    return <span key={i}>{i > 0 && i < 19 && Number(hh) % 6 === 0 ? hh : null}</span>;
  });
  return (
    <div className={`axis ${size}`} aria-hidden="true">
      {spans}
    </div>
  );
}

/** The chart as a table, for screen readers. */
export function StripTable({ hourly, from, label }: { hourly: number[]; from: string; label: string }) {
  const start = Date.parse(from);
  return (
    <table className="sr-only">
      <caption>{label}: events per hour, last 24 hours, UTC</caption>
      <thead>
        <tr>
          <th scope="col">Hour starting</th>
          <th scope="col">Events</th>
        </tr>
      </thead>
      <tbody>
        {hourly.map((n, i) => (
          <tr key={i}>
            <th scope="row">{hourRange(start + i * HOUR)}</th>
            <td>{n}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export function Legend() {
  return (
    <div className="legend">
      <h2>Events per hour</h2>
      <div className="sw" aria-hidden="true">
        {LEGEND.map((_, i) => (
          <i key={i} className={`s${i}`} />
        ))}
      </div>
      <div className="lb" aria-hidden="true">
        {LEGEND.map((t) => (
          <span key={t}>{t}</span>
        ))}
      </div>
    </div>
  );
}

const tip = () => document.getElementById("tip");

function readout(cell: HTMLElement, x?: number) {
  const el = tip();
  if (!el) return;
  const n = Number(cell.dataset.n);
  const [when, count, word] = el.children;
  if (when) when.textContent = hourRange(Number(cell.dataset.start));
  if (count) count.textContent = `${n}`;
  if (word) word.textContent = ` ${n === 1 ? "event" : "events"}`;
  el.classList.add("show");
  const r = cell.getBoundingClientRect();
  const left = Math.min(window.innerWidth - el.offsetWidth - 8, Math.max(8, (x ?? r.left + r.width / 2) - el.offsetWidth / 2));
  let top = r.top - el.offsetHeight - 8;
  if (top < 4) top = r.bottom + 8;
  el.style.left = `${left}px`;
  el.style.top = `${top}px`;
}

/** Marks hour `i` in every small strip, and `cell` itself. */
function hot(i: number, cell?: HTMLElement) {
  for (const c of document.querySelectorAll(".cell.col, .cell.on")) c.classList.remove("col", "on");
  if (i < 0) return;
  const start = cell?.dataset.start;
  if (start) for (const c of document.querySelectorAll(`.strip.sm .cell[data-start="${start}"]`)) c.classList.add("col");
  cell?.classList.add("on");
}

const cellOf = (t: EventTarget | null) => (t instanceof Element ? t.closest<HTMLElement>(".cell") : null);

/** The readout element and its listeners. Render once, in the shell. */
export function Tip() {
  useEffect(() => {
    const over = (e: PointerEvent) => {
      const c = cellOf(e.target);
      if (!c) return;
      readout(c, e.clientX);
      hot(0, c);
    };
    const out = (e: PointerEvent) => {
      if (!cellOf(e.target) || cellOf(e.relatedTarget)) return;
      tip()?.classList.remove("show");
      hot(-1);
    };
    const move = (e: PointerEvent) => {
      const c = cellOf(e.target);
      if (c && tip()?.classList.contains("show")) readout(c, e.clientX);
    };
    document.addEventListener("pointerover", over);
    document.addEventListener("pointerout", out);
    document.addEventListener("pointermove", move);
    return () => {
      document.removeEventListener("pointerover", over);
      document.removeEventListener("pointerout", out);
      document.removeEventListener("pointermove", move);
    };
  }, []);
  return (
    <div id="tip" role="presentation">
      <div className="m" />
      <b />
      <span />
    </div>
  );
}
