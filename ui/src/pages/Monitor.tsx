import { useQuery } from "@tanstack/react-query";
import { Link, getRouteApi, useNavigate } from "@tanstack/react-router";
import { useEffect } from "react";
import { Refused, get } from "../api/client";
import type { Check, Incident, MonitorDetail } from "../api/types";
import { useKeys } from "../lib/keys";
import { useNow } from "../lib/live";
import { shown } from "../lib/text";
import { agoLong, every, lasted, stamp } from "../lib/time";
import { Axis } from "../parts/HeatStrip";
import { CheckMark, StateMark } from "../parts/Marks";
import { ResponseStrip, ResponseTable, slowestWords } from "../parts/Uptime";
import { Shell, type KeyHint } from "./Shell";

const route = getRouteApi("/monitors/$id");

const KEYS: KeyHint[] = [["Esc", null, "back to uptime"]];

/** A share as a percent, never rounded up, so 99.999% does not read as 100%. */
export function pct(share: number | null): string {
  if (share === null) return "no data yet";
  if (share >= 1) return "100%";
  return `${(Math.floor(share * 10_000) / 100).toFixed(2)}%`;
}

export function Monitor() {
  const { id } = route.useParams();
  const navigate = useNavigate();
  const now = useNow();
  const view = useQuery({
    queryKey: ["monitor", id],
    queryFn: () => get<MonitorDetail>(`/monitors/${encodeURIComponent(id)}`),
    refetchInterval: 30_000,
    retry: (count, error) => !(error instanceof Refused && error.status === 404) && count < 3,
  });
  const m = view.data?.monitor;

  useEffect(() => {
    document.title = `${m ? shown(m.name) : `Monitor ${id}`} - Davy`;
  }, [m, id]);

  useKeys((e) => {
    if (e.key !== "Escape") return;
    e.preventDefault();
    void navigate({ to: "/monitors" });
  });

  const crumb = (
    <>
      <Link className="up" to="/monitors">
        Uptime
      </Link>
      <span className="sep" aria-hidden="true" />
      <b>{m ? shown(m.name) : `Monitor ${id}`}</b>
    </>
  );

  return (
    <Shell crumb={crumb} keys={KEYS} inspector={false}>
      <main className="page" aria-label={m ? `Monitor ${shown(m.name)}` : `Monitor ${id}`}>
        {view.error ? (
          view.error instanceof Refused && view.error.status === 404 ? (
            <p className="none">
              There is no monitor {id}. <Link to="/monitors">See every monitor</Link>
            </p>
          ) : (
            <p className="none">{view.error.message}</p>
          )
        ) : !view.data || !m ? (
          <p className="none">Loading monitor {id}</p>
        ) : (
          <>
            <header className="ihead">
              <div>
                <div className="tags">
                  <StateMark state={m.state} />
                  <span className="pj">{shown(m.project)}</span>
                </div>
                <h1>{shown(m.name)}</h1>
                <div className="sub">
                  {m.kind === "http" ? (
                    <>
                      <span className="fn">{m.url === null ? "no URL" : shown(m.url)}</span>
                      <span>
                        checked every {every(m.every_s)}
                        {view.data.timeout_s === null ? "" : `, timeout ${every(view.data.timeout_s)}`}
                      </span>
                    </>
                  ) : (
                    <span>
                      {m.slug === null ? `Heartbeat. A ping every ${every(m.every_s)}` : `Check-ins for ${shown(m.slug)}, ${m.schedule === null ? "no schedule" : shown(m.schedule)}`}
                      {view.data.grace_s === null ? "" : `, late after ${every(view.data.grace_s)} more`}
                    </span>
                  )}
                </div>
              </div>
            </header>
            <Summary view={view.data} now={now} />
            <Incidents incidents={view.data.incidents} now={now} />
            {m.kind === "http" ? <Checks checks={view.data.checks} /> : null}
          </>
        )}
      </main>
    </Shell>
  );
}

function Summary({ view, now }: { view: MonitorDetail; now: number }) {
  const m = view.monitor;
  const slow = slowestWords(m.hourly, view.hours_from);
  const open = view.incidents.find((i) => i.resolved === null);
  return (
    <div className="sumrow">
      <div className="panel">
        <h2>
          Response time, last 24 hours<span>{slow ?? "no timed checks"}</span>
        </h2>
        <ResponseStrip hourly={m.hourly} from={view.hours_from} size="lg" />
        <Axis from={view.hours_from} size="lg" />
        <ResponseTable hourly={m.hourly} from={view.hours_from} label={shown(m.name)} />
      </div>
      <div className="panel">
        <h2>Uptime</h2>
        <dl className="kv tonal">
          <dt>7 days</dt>
          <dd>
            <b>{pct(view.uptime_7d)}</b>
          </dd>
          <dt>30 days</dt>
          <dd>
            <b>{pct(view.uptime_30d)}</b>
          </dd>
          {m.kind === "http" ? (
            <>
              <dt>Last check</dt>
              <dd>{m.last_heard ? <When iso={m.last_heard} now={now} /> : "never"}</dd>
            </>
          ) : (
            <>
              <dt>Last ping</dt>
              <dd>{view.last_ping ? <When iso={view.last_ping} now={now} /> : "never"}</dd>
              <dt>Late after</dt>
              <dd>{view.expected_by ? <span className="mono">{stamp(view.expected_by)}</span> : "no ping expected"}</dd>
            </>
          )}
          <dt>Incident</dt>
          <dd>
            {open ? (
              <>
                open since <span className="mono">{stamp(open.started)}</span>, <span className="problem">{lasted(now - Date.parse(open.started))} so far</span>
              </>
            ) : (
              "none open"
            )}
          </dd>
        </dl>
      </div>
    </div>
  );
}

function When({ iso, now }: { iso: string; now: number }) {
  return (
    <>
      <span className="mono">{stamp(iso)}</span> <span className="dim">{agoLong(iso, now)}</span>
    </>
  );
}

function Incidents({ incidents, now }: { incidents: Incident[]; now: number }) {
  return (
    <section className="sec gap" aria-labelledby="incidents">
      <h3 id="incidents">
        Incidents<span>{incidents.length ? `newest ${incidents.length}` : "none yet"}</span>
      </h3>
      {incidents.length ? (
        <table className="iss plain">
          <colgroup>
            <col className="w-when" />
            <col className="w-when" />
            <col className="w-lasted" />
            <col />
          </colgroup>
          <thead>
            <tr>
              <th scope="col">Started</th>
              <th scope="col">Resolved</th>
              <th scope="col" className="r">
                Lasted
              </th>
              <th scope="col">Reason</th>
            </tr>
          </thead>
          <tbody>
            {incidents.map((i) => (
              <tr key={i.id}>
                <td className="mono">{stamp(i.started)}</td>
                <td>{i.resolved ? <span className="mono">{stamp(i.resolved)}</span> : <span className="problem">open</span>}</td>
                <td className="r">
                  {lasted((i.resolved ? Date.parse(i.resolved) : now) - Date.parse(i.started))}
                  {i.resolved ? "" : " so far"}
                </td>
                <td title={shown(i.reason)}>{shown(i.reason)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}
    </section>
  );
}

function Checks({ checks }: { checks: Check[] }) {
  return (
    <section className="sec" aria-labelledby="checks">
      <h3 id="checks">
        Checks<span>{checks.length ? `newest ${checks.length}` : "none yet"}</span>
      </h3>
      {checks.length ? (
        <table className="iss plain">
          <colgroup>
            <col className="w-when" />
            <col className="w-result" />
            <col className="w-code" />
            <col className="w-ms" />
            <col />
          </colgroup>
          <thead>
            <tr>
              <th scope="col">Time</th>
              <th scope="col">Result</th>
              <th scope="col" className="r">
                Status
              </th>
              <th scope="col" className="r">
                Response
              </th>
              <th scope="col">Reason</th>
            </tr>
          </thead>
          <tbody>
            {checks.map((c) => (
              <tr key={c.at}>
                <td className="mono">{stamp(c.at)}</td>
                <td>
                  <CheckMark ok={c.ok} />
                </td>
                <td className="r">{c.status ?? <span className="dim">none</span>}</td>
                <td className="r">{c.ms === null ? <span className="dim">none</span> : `${c.ms} ms`}</td>
                <td title={c.reason ? shown(c.reason) : undefined}>{c.reason ? shown(c.reason) : <span className="dim">none</span>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : null}
    </section>
  );
}
