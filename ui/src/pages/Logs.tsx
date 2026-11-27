import { keepPreviousData, useInfiniteQuery, useQuery, useQueryClient, type InfiniteData } from "@tanstack/react-query";
import { getRouteApi, useNavigate } from "@tanstack/react-router";
import { useCallback, useEffect, useMemo, useRef, useState, type Ref } from "react";
import { Refused, get, query } from "../api/client";
import type { LogHistogram, LogPage, Project } from "../api/types";
import { useKeys } from "../lib/keys";
import { announce } from "../lib/live";
import { plural } from "../lib/text";
import { msOf, nsOf, sameDay, span } from "../lib/time";
import { Histogram } from "../parts/Histogram";
import { Brand } from "../parts/Icon";
import { LogRowView, sameness, type LogRow } from "../parts/LogRows";
import { LEVELS, RANGES, type LogsSearch, type Range } from "../search";
import { RailButton, Shell, type KeyHint } from "./Shell";

const route = getRouteApi("/logs");

const KEYS: KeyHint[] = [
  ["j", "k", "move"],
  ["Enter", null, "open"],
  ["/", null, "search"],
  ["l", null, "live"],
  ["Esc", null, "zoom out"],
];

const DEFAULT_RANGE: Range = "1h";
const LIVE_EVERY_MS = 2000;
/** How often live redraws the histogram, which reads more than a page. */
const LIVE_HISTOGRAM_MS = 10_000;
/** Live asks again for the last 2 s, for logs that were committed after their time. */
const OVERLAP_NS = 2_000_000_000n;
/** Rows live may pile onto the first page before the list starts over from the newest. */
const LIVE_KEEP = 500;

type Pages = InfiniteData<LogPage, string | undefined>;

const spanOf = (range: Range | undefined) => RANGES.find(([r]) => r === (range ?? DEFAULT_RANGE))?.[2] ?? 3_600_000;

/** The newest first, by time in nanoseconds. */
const newestFirst = (a: { timestamp_ns: string }, b: { timestamp_ns: string }) => {
  const d = BigInt(b.timestamp_ns) - BigInt(a.timestamp_ns);
  return d > 0n ? 1 : d < 0n ? -1 : 0;
};

/** The logs of `fresh` that `page` lacks, merged into it newest first. */
function merge(page: LogPage, fresh: LogPage): { page: LogPage; added: string[] } {
  const have = new Set(page.logs.map(sameness));
  const added = fresh.logs.filter((l) => !have.has(sameness(l)));
  return { page: { ...page, logs: [...added, ...page.logs].sort(newestFirst) }, added: added.map(sameness) };
}

export function Logs() {
  const search = route.useSearch();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const live = search.live === "1";
  const zoomed = search.from !== undefined && search.to !== undefined;
  const filter = { q: search.q, project: search.project, level: search.level };
  const filterKey = JSON.stringify([filter, search.range, search.from, search.to, live]);

  // A range that ends now is read as of the moment it was picked, so the list
  // and the histogram agree. Refresh and the filters move that moment.
  const [refreshed, setRefreshed] = useState(0);
  const anchor = useMemo(() => Date.now(), [filterKey, refreshed]);
  const [liveAt, setLiveAt] = useState(anchor);
  useEffect(() => setLiveAt(anchor), [anchor]);
  const windowAt = (at: number) => ({ from: String(nsOf(at - spanOf(search.range))), to: String(nsOf(at)) });
  const range = zoomed ? { from: search.from, to: search.to } : windowAt(anchor);
  // Live pages older logs from now, so the page before a live page joins on.
  const listParams = { ...filter, from: range.from, to: live ? undefined : range.to };
  const histParams = { ...filter, ...(zoomed ? range : windowAt(live ? liveAt : anchor)) };

  const key = ["logs", listParams];
  const list = useInfiniteQuery({
    queryKey: key,
    queryFn: ({ pageParam }) => get<LogPage>(`/logs${query({ ...listParams, before: pageParam })}`),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.before ?? undefined,
    placeholderData: keepPreviousData,
    staleTime: Infinity,
    refetchOnWindowFocus: false,
  });
  const hist = useQuery({
    queryKey: ["logs-histogram", histParams],
    queryFn: () => get<LogHistogram>(`/logs/histogram${query(histParams)}`),
    placeholderData: keepPreviousData,
    staleTime: Infinity,
    refetchOnWindowFocus: false,
  });

  const [fresh, setFresh] = useState<ReadonlySet<string>>(new Set());
  const rows: LogRow[] = useMemo(() => {
    const count = new Map<string, number>();
    return (list.data?.pages ?? []).flatMap((p) =>
      p.logs.map((l) => {
        const same = sameness(l);
        const n = count.get(same) ?? 0;
        count.set(same, n + 1);
        return { ...l, key: n === 0 ? same : `${same}|${n}`, fresh: fresh.has(same) };
      }),
    );
  }, [list.data, fresh]);

  const [cursor, setCursor] = useState<string | null>(null);
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  const [liveProblem, setLiveProblem] = useState<string | null>(null);
  const searchBox = useRef<HTMLInputElement>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const sentinel = useRef<HTMLLIElement>(null);
  const scrollTo = useRef(false);
  const cur = rows.some((r) => r.key === cursor) ? cursor : (rows[0]?.key ?? null);

  useEffect(() => {
    document.title = "Logs - Davy";
    setOpen(new Set());
    setFresh(new Set());
  }, [filterKey]);

  const go = useCallback(
    (change: Partial<LogsSearch>, replace = false) => void navigate({ to: "/logs", search: (s: LogsSearch) => ({ ...s, ...change }), replace }),
    [navigate],
  );

  // Live: the logs after the newest one, every 2 s while the tab is in view.
  const newest = useRef<string | null>(null);
  newest.current = rows[0]?.timestamp_ns ?? null;
  useEffect(() => {
    if (!live) return;
    let busy = false;
    let stopped = false;
    const start = nsOf(anchor);
    const tick = async () => {
      if (busy || document.visibilityState !== "visible") return;
      busy = true;
      try {
        const after = (newest.current === null ? start : BigInt(newest.current)) - OVERLAP_NS;
        const page = await get<LogPage>(`/logs${query({ ...filter, from: listParams.from, after: String(after > 0n ? after : 0n) })}`);
        if (stopped) return;
        setLiveProblem(null);
        const old = qc.getQueryData<Pages>(key);
        const first = old?.pages[0];
        if (!old || !first || page.before !== null) {
          qc.setQueryData<Pages>(key, { pages: [page], pageParams: [undefined] });
          setFresh(new Set(page.logs.map(sameness)));
          return;
        }
        const { page: merged, added } = merge(first, page);
        if (added.length === 0) return;
        qc.setQueryData<Pages>(key, { ...old, pages: [merged, ...old.pages.slice(1)] });
        setFresh((f) => new Set([...f, ...added]));
        if (merged.logs.length > LIVE_KEEP) {
          qc.setQueryData<Pages>(key, (d) => d && { pages: d.pages.slice(0, 1), pageParams: d.pageParams.slice(0, 1) });
          void qc.refetchQueries({ queryKey: key });
        }
      } catch (e) {
        if (!stopped) setLiveProblem(e instanceof Error ? e.message : "Live stopped");
      } finally {
        busy = false;
      }
    };
    const t = window.setInterval(() => void tick(), LIVE_EVERY_MS);
    const h = window.setInterval(() => {
      if (document.visibilityState === "visible") setLiveAt(Date.now());
    }, LIVE_HISTOGRAM_MS);
    return () => {
      stopped = true;
      window.clearInterval(t);
      window.clearInterval(h);
    };
    // The key and the filter are new objects on every render; filterKey and anchor name them.
  }, [live, filterKey, anchor]);

  const toggleLive = () => go(live ? { live: undefined } : { live: "1", from: undefined, to: undefined });
  const zoom = (from: string, to: string) => {
    go({ from, to, live: undefined });
    announce(`Zoomed to ${span(msOf(from), msOf(to), true)}`);
  };
  const zoomOut = () => {
    go({ from: undefined, to: undefined });
    announce("Zoomed out");
  };
  const toggle = useCallback((k: string) => {
    setCursor(k);
    setOpen((o) => {
      const next = new Set(o);
      if (!next.delete(k)) next.add(k);
      return next;
    });
  }, []);
  const addTerm = useCallback(
    (term: string) => {
      go({ q: search.q ? `${search.q} AND ${term}` : term });
      announce(`Searching for ${term}`);
    },
    [go, search.q],
  );
  const move = (by: number) => {
    const at = rows.findIndex((r) => r.key === cur);
    const next = rows[Math.max(0, Math.min(rows.length - 1, at + by))];
    if (!next) return;
    scrollTo.current = true;
    setCursor(next.key);
    if (next === rows.at(-1) && list.hasNextPage && !list.isFetchingNextPage) void list.fetchNextPage();
  };

  useKeys((e) => {
    const onControl = e.target instanceof Element && e.target.closest("a, button, summary, svg");
    if (e.key === "j" || e.key === "ArrowDown") move(1);
    else if (e.key === "k" || e.key === "ArrowUp") move(-1);
    else if ((e.key === "Enter" || e.key === "o") && !onControl && cur !== null) toggle(cur);
    else if (e.key === "/") searchBox.current?.focus();
    else if (e.key === "l") toggleLive();
    else if (e.key === "Escape" && zoomed) zoomOut();
    else return;
    e.preventDefault();
  });

  useEffect(() => {
    if (!scrollTo.current) return;
    scrollTo.current = false;
    scroller.current?.querySelector(`li[data-key="${CSS.escape(cur ?? "")}"]`)?.scrollIntoView({ block: "nearest" });
  }, [cur]);

  const { hasNextPage, isFetchingNextPage, fetchNextPage } = list;
  useEffect(() => {
    const el = sentinel.current;
    if (!el || !hasNextPage) return;
    const seen = new IntersectionObserver(
      ([entry]) => {
        if (entry?.isIntersecting && !isFetchingNextPage) void fetchNextPage();
      },
      { root: scroller.current, rootMargin: "300px" },
    );
    seen.observe(el);
    return () => seen.disconnect();
  }, [hasNextPage, isFetchingNextPage, fetchNextPage]);

  const problem = list.error ?? hist.error;
  const total = hist.data?.counts.reduce((a, b) => a + b, 0);
  const from = msOf(range.from ?? "0");
  const days = !sameDay(from, msOf(range.to ?? String(nsOf(Date.now()))));

  return (
    <Shell keys={KEYS} note="Times are UTC" rail={(close) => <LogsRail search={search} zoomed={zoomed} onPick={close} />} inspector={false}>
      <main id="vlogs" aria-label="Logs" aria-busy={list.isFetching || hist.isFetching}>
        <div className="tb">
          <QueryBox ref={searchBox} value={search.q} onSet={(q) => go({ q }, true)} />
          <div className="tbr">
            {zoomed ? (
              <button type="button" className="btn" onClick={zoomOut}>
                Zoom out
              </button>
            ) : live ? null : (
              <button type="button" className="btn" onClick={() => setRefreshed((n) => n + 1)}>
                Refresh
              </button>
            )}
            <button type="button" className="btn live" aria-pressed={live} onClick={toggleLive}>
              Live<kbd>l</kbd>
            </button>
          </div>
        </div>
        <div className="hhead">
          <span className="cnt">{total === undefined ? "Counting logs" : `${total.toLocaleString("en-US")} ${plural(total, "log")}`}</span>
          <span className="when">{zoomed ? span(from, msOf(range.to ?? "0"), true) : `last ${RANGES.find(([r]) => r === (search.range ?? DEFAULT_RANGE))?.[1]}`}</span>
          {live ? <span className="when">{liveProblem ? <span className="problem">{liveProblem}</span> : "live, checks every 2 s"}</span> : null}
          <span className="hint">Drag across bars, or pick one, to zoom</span>
        </div>
        {problem ? (
          <p className="problem qerr" role="alert">
            {problem instanceof Refused && problem.status === 400 ? `The search does not read: ${problem.message}` : problem.message}
          </p>
        ) : null}
        {hist.data ? <Histogram data={hist.data} onZoom={zoom} /> : <div className="hist" />}
        <div className="tscroll" ref={scroller}>
          <ol className={days ? "lrows days" : "lrows"} aria-label="Logs, newest first">
            {rows.map((r) => (
              <LogRowView
                key={r.key}
                row={r}
                cur={r.key === cur}
                open={open.has(r.key)}
                project={!search.project}
                days={days}
                onToggle={toggle}
                onFilter={addTerm}
              />
            ))}
            {rows.length === 0 && !problem ? (
              <li className="empty">
                {list.isPending ? null : <Brand size={28} />}
                {list.isPending ? "Loading logs" : "No logs match in this range. Widen the range or clear the search."}
              </li>
            ) : null}
            {hasNextPage ? (
              <li ref={sentinel} className="more">
                <button type="button" className="btn" disabled={isFetchingNextPage} onClick={() => void fetchNextPage()}>
                  {isFetchingNextPage ? "Loading older logs" : "Load older logs"}
                </button>
              </li>
            ) : null}
          </ol>
        </div>
      </main>
    </Shell>
  );
}

const LEVEL_PICKS: [string | undefined, string][] = [
  [undefined, "All levels"],
  ...LEVELS.slice(1).map((l): [string, string] => [l, l === "fatal" ? "Fatal" : `${l[0]?.toUpperCase()}${l.slice(1)} and up`]),
];

/** Examples of the query syntax. The rail may break them after a dot. */
const SYNTAX = ["timeout", '"connect ETIMEDOUT"', "attributes.res.statusCode:500", "error AND NOT attributes.req.method:get"];

function LogsRail({ search, zoomed, onPick }: { search: LogsSearch; zoomed: boolean; onPick: () => void }) {
  const navigate = useNavigate();
  const projects = useQuery({ queryKey: ["projects"], queryFn: () => get<Project[]>("/projects"), staleTime: 60_000 });
  const pick = (change: Partial<LogsSearch>) => {
    onPick();
    void navigate({ to: "/logs", search: { ...search, ...change } });
  };
  const range = search.range ?? DEFAULT_RANGE;
  return (
    <>
      <div className="rgroup">
        <h2>Range</h2>
        {RANGES.map(([r, label]) => (
          <RailButton key={r} label={`Last ${label}`} on={!zoomed && range === r} onClick={() => pick({ range: r === DEFAULT_RANGE ? undefined : r, from: undefined, to: undefined })} />
        ))}
        {zoomed ? <RailButton label="Zoomed in" on onClick={() => pick({ from: undefined, to: undefined })} /> : null}
      </div>
      <div className="rgroup">
        <h2>Projects</h2>
        <RailButton label="All projects" on={!search.project} onClick={() => pick({ project: undefined })} />
        {projects.data?.map((p) => (
          <RailButton key={p.slug} label={p.slug} on={search.project === p.slug} onClick={() => pick({ project: p.slug })} />
        ))}
      </div>
      <div className="rgroup">
        <h2>Level</h2>
        {LEVEL_PICKS.map(([value, label]) => (
          <RailButton key={label} label={label} on={search.level === value} onClick={() => pick({ level: value as LogsSearch["level"] })} />
        ))}
      </div>
      <div className="rgroup syntax">
        <h2>Search syntax</h2>
        {SYNTAX.map((example) => (
          <code key={example}>{example.split(/(?<=\.)/).flatMap((part, i) => (i === 0 ? [part] : [<wbr key={i} />, part]))}</code>
        ))}
      </div>
    </>
  );
}

/** The search, written to the URL a quarter second after the last key. */
function QueryBox({ value, onSet, ref }: { value: string | undefined; onSet: (q: string | undefined) => void; ref: Ref<HTMLInputElement> }) {
  const [text, setText] = useState(value ?? "");
  const typed = useRef(false);
  useEffect(() => {
    if (!typed.current) setText(value ?? "");
  }, [value]);
  useEffect(() => {
    if (!typed.current) return;
    const t = window.setTimeout(() => {
      typed.current = false;
      const q = text.trim() || undefined;
      if (q !== value) onSet(q);
    }, 250);
    return () => window.clearTimeout(t);
  }, [text, value, onSet]);
  return (
    <div className="search wide">
      <input
        ref={ref}
        type="search"
        value={text}
        maxLength={1000}
        placeholder="Search messages and attributes"
        aria-label="Search messages and attributes"
        autoComplete="off"
        spellCheck={false}
        onChange={(e) => {
          typed.current = true;
          setText(e.target.value);
        }}
        onKeyDown={(e) => {
          if (e.key === "Escape" || e.key === "Enter") e.currentTarget.blur();
        }}
      />
      <kbd aria-hidden="true">/</kbd>
    </div>
  );
}
