import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { get } from "../api/client";
import type { EventView, IssueView, Status } from "../api/types";
import { useSettled } from "../lib/live";
import { plural, shown } from "../lib/text";
import { agoLong, stamp } from "../lib/time";
import { StatusButtons } from "../parts/Actions";
import { topFrame, where } from "../parts/Event";
import { Axis, Strip, StripTable, peakWords } from "../parts/HeatStrip";
import { Icon } from "../parts/Icon";
import { Level, Regressed, StatusMark } from "../parts/Marks";
import type { ListRow } from "./Issues";

/** The issue under the cursor. The row shows at once, the rest once j rests. */
export function Inspector({ row, busy, onAct }: { row: ListRow | undefined; busy: boolean; onAct: (s: Status) => void }) {
  const id = useSettled(row?.id, 120);
  const key = String(id);
  const view = useQuery({ queryKey: ["issue", key], queryFn: () => get<IssueView>(`/issues/${key}`), enabled: id !== undefined });
  const latest = useQuery({
    queryKey: ["event", key, "latest"],
    queryFn: () => get<EventView>(`/issues/${key}/event`),
    enabled: id !== undefined,
  });

  if (!row) {
    return (
      <aside id="insp" aria-label="Selected issue summary">
        <p className="none">No issue under the cursor. Use j and k to move.</p>
      </aside>
    );
  }
  const ready = id === row.id;
  const issue = ready && view.data ? view.data.issue : undefined;
  const releases = ready ? view.data?.releases : undefined;
  const ev = ready ? latest.data : undefined;
  const frame = ev ? topFrame(ev) : null;
  const total = row.hourly.reduce((a, b) => a + b, 0);
  const peak = peakWords(row.hourly, row.from);
  const now = Date.now();

  return (
    <aside id="insp" aria-label="Selected issue summary">
      <div className="meta">
        <Level level={row.level} />
        <StatusMark status={row.status} />
        {issue?.status === "resolved" && issue.resolved_in_release !== null ? <span className="hint">in {shown(issue.resolved_in_release)}</span> : null}
        {issue?.regressed_at ? <Regressed /> : null}
      </div>
      <h1>{shown(row.title)}</h1>
      <div className="hint">
        {shown(row.project)}
        {ev?.environment ? ` / ${shown(ev.environment)}` : ""}
      </div>
      <div className="sect">
        <h2>Culprit</h2>
        <div className="mono">{row.culprit === null ? <span className="dim">none</span> : shown(row.culprit)}</div>
      </div>
      <div className="sect">
        <h2>
          {frame && !frame.in_app ? "Top frame" : "Top in-app frame"}
          <span>{frame ? "nearest the crash" : ""}</span>
        </h2>
        {frame ? (
          <div className="frame-mini">
            <div className="fh">
              <b>{frame.function === null ? "anonymous" : shown(frame.function)}</b>
              <span className="mono">{where(frame)}</span>
            </div>
            {frame.context_line !== null ? (
              <div className="cl mono">
                <span className="no">{frame.lineno}</span>
                <span className="tx">{shown(frame.context_line)}</span>
              </div>
            ) : null}
            {frame.minified !== null ? (
              <div className="org">
                <Icon name="smap" />
                source map from <span className="mono">{shown(frame.minified)}</span>
              </div>
            ) : null}
          </div>
        ) : (
          <div className="none">{!ready || latest.isPending ? "Loading" : latest.error ? latest.error.message : "No stack trace on the latest event."}</div>
        )}
      </div>
      <div className="sect">
        <h2>Counts</h2>
        <div className="cnt2">
          <div>
            <div className="big">{row.count.toLocaleString("en-US")}</div>
            <div className="lab">events</div>
          </div>
          <div>
            <div className="big">{total.toLocaleString("en-US")}</div>
            <div className="lab">in the last 24 hours</div>
          </div>
        </div>
        <dl className="kv gap">
          <dt>First seen</dt>
          <dd>
            {agoLong(row.first_seen, now)}
            <div className="mono dim">{stamp(row.first_seen)}</div>
          </dd>
          <dt>Last seen</dt>
          <dd>
            {agoLong(row.last_seen, now)}
            <div className="mono dim">{stamp(row.last_seen)}</div>
          </dd>
        </dl>
      </div>
      <div className="sect">
        <h2>
          Releases<span>{releases ? releases.length : ""}</span>
        </h2>
        {releases?.length === 0 ? <div className="none">Its events name no release.</div> : null}
        {releases?.map((r) => (
          <div key={r.release} className="relrow">
            <span className="mono">{shown(r.release)}</span>
            <span className="dim">
              {r.events.toLocaleString("en-US")} {plural(r.events, "event")}, {agoLong(r.last_seen, now)}
            </span>
          </div>
        ))}
      </div>
      <div className="sect">
        <h2>
          Last 24 hours<span>{total.toLocaleString("en-US")} {plural(total, "event")}</span>
        </h2>
        <Strip key={row.id} hourly={row.hourly} from={row.from} size="md" />
        <Axis from={row.from} size="md" />
        <div className="peak">{peak ? `Peak ${peak.slice(5)}` : "No events in the last 24 hours"}</div>
        <StripTable hourly={row.hourly} from={row.from} label={`Issue ${row.id}`} />
      </div>
      <div className="acts">
        <StatusButtons status={row.status} busy={busy} keys={false} onAct={onAct} />
        <Link className="link" to="/issues/$id" params={{ id: String(row.id) }}>
          Open issue<kbd>Enter</kbd>
        </Link>
      </div>
    </aside>
  );
}
