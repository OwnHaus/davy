import * as Popover from "@radix-ui/react-popover";
import { useInfiniteQuery, useQuery, type InfiniteData } from "@tanstack/react-query";
import { Link, getRouteApi, useNavigate } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { Refused, get, query } from "../api/client";
import { useStatusChange } from "../api/status";
import type { EventPage, EventView, IssueDetail, IssueView, Logs, Status } from "../api/types";
import { useKeys } from "../lib/keys";
import { announce, copy } from "../lib/live";
import { plural, shown } from "../lib/text";
import { agoLong, stamp } from "../lib/time";
import { StatusButtons, statusKey } from "../parts/Actions";
import { Breadcrumbs, Contexts, CopyButton, Exceptions, LogLines, Pairs, UserAndTrace } from "../parts/Event";
import { Axis, Strip, StripTable, peakWords } from "../parts/HeatStrip";
import { Icon } from "../parts/Icon";
import { Level, Regressed, StatusMark } from "../parts/Marks";
import { lastList, savedSort } from "../search";
import { Shell, type KeyHint } from "./Shell";

const route = getRouteApi("/issues/$id");

const KEYS: KeyHint[] = [
  ["r", null, "resolve"],
  ["i", null, "ignore"],
  ["o", null, "reopen"],
  ["[", "]", "older, newer event"],
  ["Esc", null, "back to list"],
];

/** Events up to this many keep a payload, then every hundredth, as `store` does. */
const KEPT_FIRST = 50;

const PROMPT = (id: number) =>
  `Use the Davy MCP tool issue_context for issue ${id}, find the cause and fix it, then call mark_fixed for issue ${id}.`;

const keptOf = (data: InfiniteData<EventPage> | undefined) => (data?.pages ?? []).flatMap((p) => p.events.filter((e) => e.stored));

export function Issue() {
  const { id } = route.useParams();
  const { event } = route.useSearch();
  const navigate = useNavigate();
  const path = `/issues/${encodeURIComponent(id)}`;
  const view = useQuery({ queryKey: ["issue", id], queryFn: () => get<IssueView>(path) });
  const events = useInfiniteQuery({
    queryKey: ["events", id],
    queryFn: ({ pageParam }) => get<EventPage>(`${path}/events${query({ cursor: pageParam })}`),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next ?? undefined,
  });
  const shownEvent = useQuery({
    queryKey: ["event", id, event ?? "latest"],
    queryFn: () => get<EventView>(`${path}/event${query({ event })}`),
  });
  const logs = useQuery({
    queryKey: ["logs", id, event ?? "latest"],
    queryFn: () => get<Logs>(`${path}/logs${query({ event })}`),
    enabled: shownEvent.isSuccess,
  });
  const change = useStatusChange();
  const [pop, setPop] = useState(false);

  useEffect(() => {
    document.title = `Issue ${id} - Davy`;
  }, [id]);

  const kept = keptOf(events.data);
  const at = event === undefined ? 0 : kept.findIndex((e) => e.event_id === event);
  const goEvent = (to: string | undefined) =>
    void navigate({ to: "/issues/$id", params: { id }, search: { event: to }, replace: true });
  const canOlder = at >= 0 && (at + 1 < kept.length || events.hasNextPage);
  const canNewer = at !== 0 && kept.length > 0;

  // Most events keep no payload, so an older one may be pages away.
  const older = async () => {
    let list = kept;
    let more = events.hasNextPage;
    while (at + 1 >= list.length && more) {
      const r = await events.fetchNextPage();
      list = keptOf(r.data);
      more = r.hasNextPage;
    }
    const next = list[at + 1];
    if (next) goEvent(next.event_id);
  };
  const newer = () => {
    if (at <= 1) goEvent(undefined);
    else goEvent(kept[at - 1]?.event_id);
  };

  const issue = view.data?.issue;
  const act = (status: Status, release?: string) => {
    if (!issue || change.isPending) return;
    change.mutate(release === undefined ? { issues: [issue.id], status } : { issues: [issue.id], status, release });
  };

  useKeys((e) => {
    const status = statusKey(e.key);
    if (status) act(status);
    else if (e.key === "[" && canOlder) void older();
    else if (e.key === "]" && canNewer) newer();
    else if (e.key === "Escape") void navigate({ to: "/issues", search: lastList });
    else return;
    e.preventDefault();
  });

  const crumb = (
    <>
      <Link className="up" to="/issues" search={lastList}>
        Issues
      </Link>
      <Icon name="fwd" size={11} />
      <b>#{id}</b>
    </>
  );

  return (
    <Shell crumb={crumb} keys={KEYS} filters={lastList} sort={lastList.sort ?? savedSort()} inspector={false}>
      <main id="vissue" aria-label={`Issue ${id}`}>
        {view.error ? (
          <p className="none">{view.error instanceof Refused && view.error.status === 404 ? `There is no issue ${id}.` : view.error.message}</p>
        ) : !view.data || !issue ? (
          <p className="none">Loading issue {id}</p>
        ) : (
          <>
            <header className="ihead">
              <div>
                <div className="tags">
                  <Level level={issue.level} />
                  <StatusMark status={issue.status} />
                  {issue.status === "resolved" && issue.resolved_in_release !== null ? (
                    <span className="hint">in {shown(issue.resolved_in_release)}</span>
                  ) : null}
                  {issue.regressed_at !== null ? <Regressed /> : null}
                </div>
                <h1>{shown(issue.title)}</h1>
                <div className="sub">
                  <span className="fn">{issue.culprit === null ? "no culprit" : shown(issue.culprit)}</span>
                  <span>{shown(issue.project)}</span>
                  {shownEvent.data?.environment ? <span>{shown(shownEvent.data.environment)}</span> : null}
                </div>
              </div>
              <div className="iacts">
                <StatusButtons status={issue.status} busy={change.isPending} keys onAct={(s) => act(s)} />
                <ResolveIn
                  open={pop}
                  onOpen={setPop}
                  releases={view.data.releases.map((r) => r.release)}
                  busy={change.isPending}
                  onResolve={(release) => {
                    setPop(false);
                    act("resolved", release);
                  }}
                />
                {change.error ? (
                  <span className="problem" role="alert">
                    {change.error.message}
                  </span>
                ) : null}
              </div>
            </header>
            <Summary view={view.data} />
            <div className="evbar">
              <h2>Event</h2>
              <div className="stepper">
                <button type="button" className="btn" disabled={!canOlder || events.isFetchingNextPage} aria-label="Older event" onClick={() => void older()}>
                  <Icon name="back" size={12} />
                  Older
                </button>
                <span>{stepWords(at, kept.length, events.hasNextPage)}</span>
                <button type="button" className="btn" disabled={!canNewer} aria-label="Newer event" onClick={newer}>
                  Newer
                  <Icon name="fwd" size={12} />
                </button>
              </div>
              <HandOver id={issue.id} />
            </div>
            {issue.event_count > KEPT_FIRST ? (
              <div className="evnote">
                Of its {issue.event_count.toLocaleString("en-US")} events, Davy kept the payload of the first {KEPT_FIRST} and then of every hundredth.
                The others count in the chart, without a payload to show.
              </div>
            ) : null}
            {shownEvent.data ? (
              <EventBody ev={shownEvent.data} logs={logs.data} logsError={logs.error?.message ?? null} />
            ) : (
              <p className="none aside-note">{shownEvent.error ? shownEvent.error.message : "Loading the event"}</p>
            )}
          </>
        )}
      </main>
    </Shell>
  );
}

function stepWords(at: number, kept: number, more: boolean): string {
  if (kept === 0) return "No kept event yet";
  if (at < 0) return "Event from the link";
  return `Event ${at + 1} of ${kept}${more ? "+" : ""}${at === 0 ? ", latest" : ""}`;
}

function Summary({ view }: { view: IssueView }) {
  const issue: IssueDetail = view.issue;
  const total = view.hourly.reduce((a, b) => a + b, 0);
  const peak = peakWords(view.hourly, view.hours_from);
  const now = Date.now();
  return (
    <div className="sumrow">
      <div className="panel">
        <h2>
          Last 24 hours<span>{total ? `${total} ${plural(total, "event")}${peak ? `, ${peak}` : ""}` : "no events"}</span>
        </h2>
        <Strip hourly={view.hourly} from={view.hours_from} size="lg" />
        <Axis from={view.hours_from} size="lg" />
        <StripTable hourly={view.hourly} from={view.hours_from} label={`Issue ${issue.id}`} />
      </div>
      <div className="panel">
        <h2>Summary</h2>
        <dl className="kv">
          <dt>Events</dt>
          <dd>
            <b>{issue.event_count.toLocaleString("en-US")}</b>
          </dd>
          <dt>First seen</dt>
          <dd>
            <span className="mono">{stamp(issue.first_seen)}</span> <span className="dim">{agoLong(issue.first_seen, now)}</span>
          </dd>
          <dt>Last seen</dt>
          <dd>
            <span className="mono">{stamp(issue.last_seen)}</span> <span className="dim">{agoLong(issue.last_seen, now)}</span>
          </dd>
          <dt>Releases</dt>
          <dd>
            {view.releases.length === 0 ? <span className="none">none named</span> : null}
            {view.releases.map((r) => (
              <div key={r.release} className="mono">
                {shown(r.release)} <span className="dim">{r.events.toLocaleString("en-US")}</span>
              </div>
            ))}
          </dd>
          {issue.status === "resolved" ? (
            <>
              <dt>Resolved in</dt>
              <dd className="mono">{issue.resolved_in_release === null ? "no release set" : shown(issue.resolved_in_release)}</dd>
            </>
          ) : null}
        </dl>
      </div>
    </div>
  );
}

function ResolveIn({ open, onOpen, releases, busy, onResolve }: { open: boolean; onOpen: (o: boolean) => void; releases: string[]; busy: boolean; onResolve: (release: string) => void }) {
  const input = useRef<HTMLInputElement>(null);
  const apply = () => {
    const v = input.current?.value.trim();
    if (v) onResolve(v);
  };
  return (
    <Popover.Root open={open} onOpenChange={onOpen}>
      <Popover.Trigger asChild>
        <button type="button" className="btn" disabled={busy}>
          <Icon name="resolved" />
          Resolve in release
        </button>
      </Popover.Trigger>
      <Popover.Content
        className="pop"
        side="bottom"
        align="end"
        sideOffset={6}
        aria-label="Resolve in release"
        onOpenAutoFocus={(e) => {
          e.preventDefault();
          input.current?.focus();
          input.current?.select();
        }}
      >
        <label htmlFor="relin">Resolved in release</label>
        <input
          id="relin"
          ref={input}
          list="rels"
          defaultValue={releases[0] ?? ""}
          maxLength={200}
          autoComplete="off"
          spellCheck={false}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              apply();
            }
          }}
        />
        <datalist id="rels">
          {releases.map((r) => (
            <option key={r} value={r} />
          ))}
        </datalist>
        <div className="row">
          <button type="button" className="btn" onClick={apply}>
            Resolve
          </button>
          <span className="hint">Enter to apply, Esc to close</span>
        </div>
      </Popover.Content>
    </Popover.Root>
  );
}

function HandOver({ id }: { id: number }) {
  const [said, setSaid] = useState<string | null>(null);
  return (
    <button
      type="button"
      className="btn agent push"
      onClick={async () => {
        const ok = await copy(PROMPT(id));
        setSaid(ok ? `Copied the prompt for issue ${id}` : "Could not copy");
        announce(ok ? `Prompt for issue ${id} copied to the clipboard` : "Could not copy the prompt");
        window.setTimeout(() => setSaid(null), 2200);
      }}
    >
      <Icon name="agent" size={14} />
      <span>{said ?? "Hand to an agent"}</span>
    </button>
  );
}

function EventBody({ ev, logs, logsError }: { ev: EventView; logs: Logs | undefined; logsError: string | null }) {
  return (
    <>
      <div className="evmeta">
        <span className="c">
          ID<span className="mono">{shown(ev.event_id)}</span>
          <CopyButton text={ev.event_id} label="event id" />
        </span>
        {ev.timestamp !== null ? (
          <span className="c">
            <span className="mono">{stamp(ev.timestamp)}</span>
          </span>
        ) : null}
        {ev.release !== null ? <span className="c mono">{shown(ev.release)}</span> : null}
        {ev.platform !== null ? <span className="c">{shown(ev.platform)}</span> : null}
      </div>
      <div className="evgrid">
        <div>
          <Exceptions ev={ev} eventKey={ev.event_id} />
          <Breadcrumbs cut={ev.breadcrumbs} />
          {logs ? (
            <LogLines scope={logs.scope} logs={logs.logs} />
          ) : (
            <section className="sec">
              <h3>Logs around the event</h3>
              <div className="none">{logsError ?? "Loading"}</div>
            </section>
          )}
        </div>
        <div className="side">
          <section className="sec">
            <h3>
              Tags<span>{ev.tags.items.length + ev.tags.more}</span>
            </h3>
            <Pairs cut={ev.tags} empty="No tags on this event." />
          </section>
          <Contexts cut={ev.contexts} />
          <UserAndTrace ev={ev} />
        </div>
      </div>
    </>
  );
}
