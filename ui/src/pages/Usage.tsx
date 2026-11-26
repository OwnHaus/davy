// The usage page: how full the disk is and when it fills, what each project
// stores per day, where to save, and what became of what each project sent.
// The header, projects and savings are `davy usage` with its default window,
// so the page and the CLI agree. The charts reach back 30 or 90 days.

import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { getRouteApi, useNavigate } from "@tanstack/react-router";
import { useEffect, type ReactNode } from "react";
import { get, query } from "../api/client";
import type { UsageDays } from "../api/types";
import type { Count, Savings as SavingsData, UsageProject, UsageReport } from "../api/usage";
import { bytes, plural, shown } from "../lib/text";
import { Outcomes } from "../parts/Outcomes";
import { Savings } from "../parts/Savings";
import { dayLabel } from "../parts/StackedArea";
import { Stored } from "../parts/Stored";
import type { UsageSearch } from "../search";
import { RailButton, Shell, type KeyHint } from "./Shell";

const route = getRouteApi("/usage");

const KEYS: KeyHint[] = [["←", "→", "Days in the chart"]];

const count = (n: number) => n.toLocaleString("en-US");
/** A retention: `30 days`, or `100 years` for the 36,500 days that mean keep. */
const keep = (n: number) => (n % 365 === 0 ? `${n / 365} ${plural(n / 365, "year")}` : `${n} ${plural(n, "day")}`);

/** The three reads of the page. The days come for the picked project alone, or the first eight and Other. */
function useUsage(project: string | undefined, since: string) {
  const report = useQuery({
    queryKey: ["usage", project],
    queryFn: () => get<UsageReport>(`/usage${query({ project })}`),
    staleTime: 60_000,
  });
  const savings = useQuery({
    queryKey: ["usage-savings", project],
    queryFn: () => get<SavingsData>(`/usage${query({ view: "savings", project })}`),
    staleTime: 60_000,
  });
  const days = useQuery({
    queryKey: ["usage-days", since, project],
    queryFn: () => get<UsageDays>(`/usage/days${query({ since, project })}`),
    staleTime: 60_000,
    placeholderData: keepPreviousData,
  });
  return { report, savings, days };
}

export function Usage() {
  const search = route.useSearch();
  const navigate = useNavigate();
  const project = search.project;
  const { report, savings, days } = useUsage(project, search.days === "90" ? "90d" : "30d");

  useEffect(() => {
    document.title = "Usage - Davy";
  }, []);

  const go = (change: UsageSearch) => void navigate({ to: "/usage", search: { ...search, ...change }, replace: true });
  const rail = (close: () => void) => (
    <div className="rgroup">
      <h2>Projects</h2>
      <RailButton label="All projects" on={!project} onClick={() => (close(), go({ project: undefined }))} />
      {days.data?.names.map((name) => (
        <RailButton key={name} label={shown(name)} on={project === name} onClick={() => (close(), go({ project: name }))} />
      ))}
    </div>
  );

  return (
    <Shell keys={KEYS} rail={rail} inspector={false}>
      <main className="page" aria-label="Usage" aria-busy={report.isFetching || savings.isFetching || days.isFetching}>
        <div className="col wide">
          <h1 className="ph">Usage{project ? <span className="of"> of {shown(project)}</span> : null}</h1>
          <Part id="u-disk" title="Disk" sub="the filesystem of the data directory">
            <Loaded q={report} what="the disk">
              {(r) => <DiskHeader r={r} />}
            </Loaded>
          </Part>
          <Part id="u-stored" title="Stored per day" sub="logs as stored, other signals as received, UTC days">
            <Loaded q={days} what="the days">
              {(d) => <Stored data={d} project={project} wide={search.days === "90"} onDays={(n) => go({ days: n })} />}
            </Loaded>
          </Part>
          <Part id="u-projects" title="Projects" sub={report.data ? `since ${dayLabel(report.data.window.since)}, ${report.data.window.days} days` : null}>
            <Loaded q={report} what="the projects">
              {(r) => <Projects projects={r.projects} names={days.data?.names ?? []} />}
            </Loaded>
          </Part>
          <Part id="u-savings" title="Savings" sub={savings.data ? `since ${dayLabel(savings.data.window.since)}` : null}>
            <Loaded q={savings} what="the savings">
              {(s) => <Savings data={s} project={project} />}
            </Loaded>
          </Part>
          <Part id="u-outcomes" title="What became of what was sent" sub="per project and UTC day, newest first">
            <Loaded q={days} what="the days">
              {(d) => <Outcomes data={d} project={project} />}
            </Loaded>
          </Part>
        </div>
      </main>
    </Shell>
  );
}

/** A part of the page under its heading. */
function Part({ id, title, sub, children }: { id: string; title: string; sub: string | null; children: ReactNode }) {
  return (
    <section className="sec" aria-labelledby={id}>
      <h3 id={id}>
        {title}
        <span>{sub}</span>
      </h3>
      {children}
    </section>
  );
}

/** A query's data, or what keeps it from showing. */
function Loaded<T>({ q, what, children }: { q: { data: T | undefined; error: Error | null }; what: string; children: (data: T) => ReactNode }) {
  if (q.data !== undefined) return <>{children(q.data)}</>;
  if (q.error) return <p className="none">{q.error.message}</p>;
  return <p className="none">Loading {what}</p>;
}

/** `Mar 1, 2027` for the UTC day `2027-03-01`. */
const date = (iso: string) => `${dayLabel(iso)}, ${iso.slice(0, 4)}`;

function fills(r: UsageReport): string {
  const d = r.disk;
  if (d?.past_limit) return "now: the disk is past the space Davy keeps free";
  if (r.no_forecast !== undefined || !d) return "no forecast";
  if (d.beyond_10_years) return "beyond 10 years";
  if (d.days_left === 0) return "today";
  if (d.days_left === null || !d.full_on) return "never";
  const range = r.basis?.full_on_range;
  const between = range?.[0] && range[1] && range[0] !== range[1] ? `, between ${date(range[0])} and ${date(range[1])}` : "";
  return `${date(d.full_on)}, in ${count(d.days_left)} ${plural(d.days_left, "day")}${d.worst_case ? " at the earliest" : ""}${between}`;
}

/** Used, free and the room left before the share of the disk Davy keeps free. */
function DiskRows({ d }: { d: NonNullable<UsageReport["disk"]> }) {
  const share = d.total > 0 ? Math.round((d.headroom / d.total) * 100) : 0;
  return (
    <>
      <dt>Used</dt>
      <dd>
        {bytes(d.used)} of {bytes(d.total)}
        <span className="usub">{d.total > 0 ? ` ${Math.round((d.used / d.total) * 100)}%` : ""}</span>
      </dd>
      <dt>Free</dt>
      <dd>{bytes(d.free)}</dd>
      <dt>Room left</dt>
      <dd data-testid="room">
        {d.past_limit ? "none" : bytes(d.room_left)}
        <span className="usub">
          {d.past_limit ? `, Davy keeps the last ${share}% free, ${bytes(d.headroom)}` : ` before the last ${share}%, ${bytes(d.headroom)}, which Davy keeps free`}
        </span>
      </dd>
    </>
  );
}

function DiskHeader({ r }: { r: UsageReport }) {
  const d = r.disk;
  const basis = r.basis;
  const soon = d?.past_limit || (d?.days_left != null && !d.beyond_10_years);
  return (
    <>
      <dl className="kv tonal ukv">
        {d ? <DiskRows d={d} /> : null}
        <dt>Full</dt>
        <dd data-testid="fills">{fills(r)}</dd>
        {d?.steady_state != null ? (
          <>
            <dt>Steady state</dt>
            <dd>{bytes(d.steady_state)} once every retention is full</dd>
          </>
        ) : null}
        {basis ? (
          <>
            <dt>Forecast</dt>
            <dd>
              {basis.confidence ? `${basis.confidence} confidence, ` : ""}logs {basis.logs.basis.replace(/_/g, " ")}
              {basis.errors ? `, errors ${basis.errors.basis}` : ""}
            </dd>
          </>
        ) : null}
      </dl>
      {soon ? <p className="why">To make room, free space on the disk, shorten a retention, or see the savings below.</p> : null}
      {basis ? <p className="why">{basis.why}</p> : null}
      {r.no_forecast !== undefined ? <p className="why">No forecast: {r.no_forecast}.</p> : null}
      {r.disk_error !== undefined ? <p className="none">The disk was not measured: {shown(r.disk_error)}</p> : null}
      {d?.partial ? (
        <p className="hint">
          The walk left out {count(d.partial.skipped_count)} {plural(d.partial.skipped_count, "path")}
          {d.partial.stopped ? ` and stopped early: ${shown(d.partial.stopped)}` : ""}, so sizes may be low.
        </p>
      ) : null}
    </>
  );
}

function Cell({ c, bytesOnly = false }: { c: Count; bytesOnly?: boolean }) {
  if (bytesOnly) return <td className="r">{bytes(c.bytes)}</td>;
  return (
    <td className="r">
      {count(c.records)}
      <span className="usub">{bytes(c.bytes)}</span>
    </td>
  );
}

const SIGNAL_COLUMNS: [keyof UsageProject["signals"], string][] = [
  ["errors", "Errors"],
  ["logs", "Logs"],
  ["sessions", "Sessions"],
  ["feedback", "Feedback"],
  ["checkins", "Check-ins"],
];

/** The report's projects in the order of the rail and the legend, oldest first. */
function byAge(projects: UsageProject[], names: string[]): UsageProject[] {
  const at = (slug: string) => {
    const i = names.indexOf(slug);
    return i < 0 ? names.length : i;
  };
  return [...projects].sort((a, b) => at(a.project) - at(b.project));
}

function Projects({ projects, names }: { projects: UsageProject[]; names: string[] }) {
  if (projects.length === 0) return <p className="none">No projects yet.</p>;
  return (
    <div className="tscroll" role="region" aria-labelledby="u-projects" tabIndex={0}>
      <table className="iss utable" id="uprojects">
        <thead>
          <tr>
            <th scope="col" className="lead w-project">
              Project
            </th>
            {SIGNAL_COLUMNS.map(([key, label]) => (
              <th key={key} scope="col" className="r">
                {label}
              </th>
            ))}
            {["Logs stored", "Logs filtered", "Errors discarded"].map((label) => (
              <th key={label} scope="col" className="r">
                {label}
              </th>
            ))}
            <th scope="col" className="r w-keep">
              Retention
            </th>
          </tr>
        </thead>
        <tbody>
          {byAge(projects, names).map((p) => (
            <tr key={p.project}>
              <th scope="row" className="lead">
                {shown(p.project)}
              </th>
              {SIGNAL_COLUMNS.map(([key]) => (
                <Cell key={key} c={p.signals[key]} />
              ))}
              <Cell c={p.logs_stored} bytesOnly />
              <Cell c={p.logs_filtered} />
              <td className="r">{count(p.errors_discarded)}</td>
              <td className="r">
                logs {keep(p.logs_days)}
                <span className="usub">errors {keep(p.errors_days)}</span>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
