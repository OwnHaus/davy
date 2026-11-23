import { useQuery } from "@tanstack/react-query";
import { Link, getRouteApi, useNavigate } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { get } from "../api/client";
import type { MonitorPage, MonitorRow, State } from "../api/types";
import { useKeys } from "../lib/keys";
import { useNow } from "../lib/live";
import { plural, shown } from "../lib/text";
import { ago, every, stamp } from "../lib/time";
import { Axis } from "../parts/HeatStrip";
import { StateMark } from "../parts/Marks";
import { ResponseStrip, UptimeLegend } from "../parts/Uptime";
import { RailButton, Shell, type KeyHint } from "./Shell";

const route = getRouteApi("/monitors");

const KEYS: KeyHint[] = [
  ["j", "k", "move"],
  ["Enter", null, "open"],
];

/** Worst first, so a monitor that is down is the first row. */
const ORDER: Record<State, number> = { down: 0, failing: 1, pending: 2, up: 3 };

export const monitorsQuery = { queryKey: ["monitors"], queryFn: () => get<MonitorPage>("/monitors"), refetchInterval: 30_000 };

export function Monitors() {
  const { project } = route.useSearch();
  const navigate = useNavigate();
  const now = useNow();
  const list = useQuery(monitorsQuery);
  const all = list.data?.monitors ?? [];
  const rows = all.filter((m) => !project || m.project === project).sort((a, b) => ORDER[a.state] - ORDER[b.state]);
  const projects = [...new Set(all.map((m) => m.project))].sort();
  const [cursor, setCursor] = useState<number | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const scrollTo = useRef(false);
  const cur = rows.some((r) => r.id === cursor) ? cursor : (rows[0]?.id ?? null);

  useEffect(() => {
    document.title = "Uptime - Davy";
  }, []);

  const open = (id: number | null) => {
    if (id !== null) void navigate({ to: "/monitors/$id", params: { id: String(id) } });
  };
  const move = (by: number) => {
    const at = rows.findIndex((r) => r.id === cur);
    const next = rows[Math.max(0, Math.min(rows.length - 1, at + by))];
    if (!next) return;
    scrollTo.current = true;
    setCursor(next.id);
  };

  useKeys((e) => {
    const onControl = e.target instanceof Element && e.target.closest("a, button");
    if (e.key === "j" || e.key === "ArrowDown") move(1);
    else if (e.key === "k" || e.key === "ArrowUp") move(-1);
    else if (e.key === "Enter" && !onControl) open(cur);
    else return;
    e.preventDefault();
  });

  useEffect(() => {
    if (!scrollTo.current) return;
    scrollTo.current = false;
    scroller.current?.querySelector(`tr.row[data-id="${cur}"]`)?.scrollIntoView({ block: "nearest" });
  }, [cur]);

  const down = rows.filter((r) => r.state === "down").length;
  const failing = rows.filter((r) => r.state === "failing").length;

  const rail = (close: () => void) => (
    <>
      <div className="rgroup">
        <h2>Projects</h2>
        <RailButton
          label="All projects"
          on={!project}
          onClick={() => {
            close();
            void navigate({ to: "/monitors", search: {} });
          }}
        />
        {projects.map((p) => (
          <RailButton
            key={p}
            label={p}
            on={project === p}
            onClick={() => {
              close();
              void navigate({ to: "/monitors", search: { project: p } });
            }}
          />
        ))}
      </div>
      <UptimeLegend />
    </>
  );

  return (
    <Shell keys={KEYS} note="Response bars use one scale per row" rail={rail} inspector={false}>
      <main id="vmons" className="list" aria-label="Uptime" aria-busy={list.isFetching}>
        <div className="tb">
          <h1 className="th">Uptime</h1>
          <div className="tbr">
            <span className="cnt flush">
              {rows.length} {plural(rows.length, "monitor")}
              {down ? `, ${down} down` : ""}
              {failing ? `, ${failing} failing` : ""}
            </span>
          </div>
        </div>
        <div className="tscroll" ref={scroller}>
          <table className="iss mons">
            <colgroup>
              <col className="w-state" />
              <col />
              <col className="w-target" />
              <col className="w-every" />
              <col className="w-age" />
              <col className="w-strip" />
            </colgroup>
            <thead>
              <tr>
                <th scope="col" className="lead">
                  State
                </th>
                <th scope="col">Monitor</th>
                <th scope="col">Target</th>
                <th scope="col" className="r">
                  Every
                </th>
                <th scope="col" className="r">
                  Last
                </th>
                <th scope="col" aria-label="Checks per hour, last 24 hours, hour of day in UTC">
                  {list.data ? <Axis from={list.data.hours_from} size="sm" /> : null}
                </th>
              </tr>
            </thead>
            <tbody>
              {list.data
                ? rows.map((r) => (
                    <Row key={r.id} row={r} from={list.data.hours_from} now={now} cur={r.id === cur} onCursor={setCursor} onOpen={open} />
                  ))
                : null}
              {rows.length === 0 ? (
                <tr>
                  <td colSpan={6}>
                    <Empty loading={list.isPending} error={list.error?.message ?? null} filtered={all.length > 0} />
                  </td>
                </tr>
              ) : null}
            </tbody>
          </table>
        </div>
      </main>
    </Shell>
  );
}

function Empty({ loading, error, filtered }: { loading: boolean; error: string | null; filtered: boolean }) {
  if (loading) return <div className="empty">Loading monitors</div>;
  if (error) return <div className="empty">{error}</div>;
  if (filtered) return <div className="empty">This project has no monitors.</div>;
  return (
    <div className="empty">
      <p>No monitors yet. Add one on the server. Davy checks a URL every minute and opens an incident when it fails.</p>
      <pre className="cmd">davy monitor add my-app home --url https://example.com/health --data /var/lib/davy</pre>
      <p>For a cron job, add a heartbeat with --heartbeat. The job pings the path Davy prints, and a missed ping opens an incident.</p>
    </div>
  );
}

type RowProps = {
  row: MonitorRow;
  from: string;
  now: number;
  cur: boolean;
  onCursor: (id: number) => void;
  onOpen: (id: number) => void;
};

function Row({ row, from, now, cur, onCursor, onOpen }: RowProps) {
  return (
    <tr
      className={`row${cur ? " cur" : ""}`}
      data-id={row.id}
      aria-current={cur ? "true" : undefined}
      onClick={(e) => {
        if (!(e.target instanceof Element && e.target.closest(".cell"))) onCursor(row.id);
      }}
      onDoubleClick={() => onOpen(row.id)}
    >
      <td className="lead">
        <StateMark state={row.state} />
        {row.open_since ? (
          <span className="dim since" title={`Since ${stamp(row.open_since)}`}>
            {ago(row.open_since, now)}
          </span>
        ) : null}
      </td>
      <td className="title">
        <div className="tt">
          <span className="pj">{shown(row.project)}</span>
          <Link to="/monitors/$id" params={{ id: String(row.id) }}>
            {shown(row.name)}
          </Link>
        </div>
      </td>
      <td title={row.url ?? undefined}>
        {row.url ? <span className="fn">{shown(row.url)}</span> : <span className="dim">{heartbeatTarget(row)}</span>}
      </td>
      <td className="r">{row.every_s > 0 ? every(row.every_s) : "calendar"}</td>
      <td className="r rel" title={row.last_heard ? stamp(row.last_heard) : undefined}>
        {row.last_heard ? ago(row.last_heard, now) : "never"}
      </td>
      <td>
        <ResponseStrip hourly={row.hourly} from={from} size="sm" />
      </td>
    </tr>
  );
}

/** A heartbeat pinged by URL, or one that takes Sentry SDK check-ins. */
function heartbeatTarget(row: MonitorRow): string {
  if (row.slug === null) return `heartbeat, a ping every ${every(row.every_s)}`;
  return `check-ins for ${shown(row.slug)}, ${row.schedule === null ? "no schedule" : shown(row.schedule)}`;
}
