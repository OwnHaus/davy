// What became of what each project sent, per UTC day: what Davy accepted,
// the parts of it it then did not store, what it refused past a limit,
// partly accepted first, and what never reached it. Only
// the rows where something arrived, the newest two weeks unless asked.

import { useState } from "react";
import type { Counts, UsageDays } from "../api/types";
import { plural, shown } from "../lib/text";
import { dayLabel } from "./StackedArea";

type Outcome = keyof Omit<Counts, "stored">;

const NOT_STORED: [Outcome, string][] = [
  ["filtered", "Filtered"],
  ["discarded", "Discarded"],
  ["refused", "Refused"],
];
const LIMITED: [Outcome, string] = ["limited", "Rate limited"];
const NEVER: [Outcome, string][] = [
  ["unreadable", "Unreadable"],
  ["sdk_dropped", "Dropped by SDK"],
];
const ALL: [Outcome, string][] = [["accepted", "Accepted"], ...NOT_STORED, LIMITED, ...NEVER];

/** The days shown unless every day is asked for. */
const RECENT = 14;
/** The most rows the table draws. */
const MOST = 300;

const count = (n: number) => n.toLocaleString("en-US");

type Group = { key: string; name: string; most: string | null; counts: Counts };
type Row = { day: string; i: number; g: Group };

function groups(d: UsageDays): Group[] {
  const out: Group[] = d.projects.map((p) => ({ key: `p:${p.project}`, name: shown(p.project), most: null, counts: p }));
  const o = d.other;
  if (o) out.push({ key: "", name: `Other, ${o.projects} ${plural(o.projects, "project")}`, most: o.top.map((t) => shown(t.project)).join(", ") || null, counts: o });
  return out;
}

/** The rows where something arrived, newest day first, of the last `days` days. */
function rows(d: UsageDays, days: number): Row[] {
  const gs = groups(d);
  const from = Math.max(0, d.days.length - days);
  return d.days
    .map((day, i) => ({ day, i }))
    .slice(from)
    .reverse()
    .flatMap(({ day, i }) => gs.filter((g) => ALL.some(([k]) => (g.counts[k][i] ?? 0) > 0)).map((g) => ({ day, i, g })));
}

export function Outcomes({ data, project }: { data: UsageDays; project: string | undefined }) {
  const [every, setEvery] = useState(false);
  const longer = data.days.length > RECENT;
  const all = rows(data, every || !longer ? data.days.length : RECENT);
  return (
    <>
      <p className="lede">
        Accepted counts every item Davy took in. Filtered, discarded and refused are parts of it that Davy then did not store. Rate limited counts items past a rate limit or
        daily cap, which Davy accepted first, and items past a project&apos;s quota or spike rate, which it never accepted. Unreadable items and those the SDKs dropped never reached it.
      </p>
      <p className="hint">These count items. What an SDK reports in bytes or seconds is left out.</p>
      {longer ? (
        <div className="ctl">
          <button type="button" className="btn" aria-pressed={every} onClick={() => setEvery((e) => !e)}>
            {every ? `Show the newest ${RECENT} days` : `Show all ${data.days.length} days`}
          </button>
        </div>
      ) : null}
      {all.length === 0 ? (
        <p className="none">Nothing arrived in {every || !longer ? "these days" : `the newest ${RECENT} days`}.</p>
      ) : (
        <Table rows={all.slice(0, MOST)} project={project} />
      )}
      {all.length > MOST ? (
        <p className="hint">
          Showing the first {MOST} of {count(all.length)} rows. Pick a project to see fewer.
        </p>
      ) : null}
    </>
  );
}

function Head({ project }: { project: string | undefined }) {
  return (
    <thead>
      <tr>
        <th scope="col" rowSpan={2} className="lead w-day">
          Day, UTC
        </th>
        {project ? null : (
          <th scope="col" rowSpan={2} className="w-project">
            Project
          </th>
        )}
        <th scope="col" rowSpan={2} className="r">
          Accepted
        </th>
        <th scope="colgroup" colSpan={NOT_STORED.length} className="grp">
          Of those, not stored
        </th>
        <th scope="col" rowSpan={2} className="r">
          {LIMITED[1]}
        </th>
        <th scope="colgroup" colSpan={NEVER.length} className="grp">
          Never accepted
        </th>
      </tr>
      <tr>
        {[...NOT_STORED, ...NEVER].map(([k, label]) => (
          <th key={k} scope="col" className="r">
            {label}
          </th>
        ))}
      </tr>
    </thead>
  );
}

function Table({ rows, project }: { rows: Row[]; project: string | undefined }) {
  return (
    <div className="tscroll sbox" role="region" aria-labelledby="u-outcomes" tabIndex={0}>
      <table className="iss otable" id="uoutcomes">
        <colgroup span={project ? 2 : 3} />
        <colgroup span={NOT_STORED.length} className="cg" />
        <colgroup span={1} className="cg" />
        <colgroup span={NEVER.length} className="cg" />
        <Head project={project} />
        <tbody>
          {rows.map(({ day, i, g }) => (
            <tr key={`${day}-${g.key}`}>
              <th scope="row" className="lead">
                {dayLabel(day)}
              </th>
              {project ? null : (
                <td>
                  {g.name}
                  {g.most ? <span className="usub">most: {g.most}</span> : null}
                </td>
              )}
              {ALL.map(([k]) => {
                const n = g.counts[k][i] ?? 0;
                return (
                  <td key={k} className={n === 0 ? "r z" : "r"}>
                    {count(n)}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
