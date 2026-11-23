import { keepPreviousData, useInfiniteQuery, useQueryClient, type InfiniteData } from "@tanstack/react-query";
import { Link, getRouteApi, useNavigate } from "@tanstack/react-router";
import { memo, useEffect, useMemo, useRef, useState, type Ref } from "react";
import { get, query } from "../api/client";
import { useStatusChange } from "../api/status";
import type { IssuePage, IssueRow, Sort, Status } from "../api/types";
import { useKeys } from "../lib/keys";
import { reducedMotion, useNow } from "../lib/live";
import { plural, shown } from "../lib/text";
import { ago, stamp } from "../lib/time";
import { StatusButtons, statusKey } from "../parts/Actions";
import { Axis, Strip } from "../parts/HeatStrip";
import { Icon } from "../parts/Icon";
import { Level, StatusMark } from "../parts/Marks";
import { rememberList, saveSort, savedSort, type IssuesSearch } from "../search";
import { Inspector } from "./Inspector";
import { IssuesRail, Shell, type KeyHint } from "./Shell";

const route = getRouteApi("/issues");

/** A row with the start of its strip, which comes with its page. */
export type ListRow = IssueRow & { from: string };

const KEYS: KeyHint[] = [
  ["j", "k", "move"],
  ["Enter", null, "open"],
  ["x", null, "select"],
  ["r", null, "resolve"],
  ["i", null, "ignore"],
  ["o", null, "reopen"],
  ["/", null, "search"],
];

const COLUMNS: [Sort, string][] = [
  ["events", "Events"],
  ["first_seen", "First"],
  ["last_seen", "Last"],
];

/** How long a row that no longer matches takes to fade, as in the CSS. */
const FADE_MS = 170;

function without(set: ReadonlySet<number>, ids: number[]): Set<number> {
  const next = new Set(set);
  for (const id of ids) next.delete(id);
  return next;
}

function keepRows(data: InfiniteData<IssuePage> | undefined, keep: (r: IssueRow) => IssueRow | null) {
  if (!data) return data;
  return {
    ...data,
    pages: data.pages.map((p) => ({ ...p, issues: p.issues.flatMap((r) => keep(r) ?? []) })),
  };
}

export function Issues() {
  const search = route.useSearch();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const now = useNow();
  const sort = search.sort ?? savedSort();
  const params = {
    sort,
    project: search.project,
    status: search.status === "all" ? undefined : (search.status ?? "unresolved"),
    environment: search.environment,
    release: search.release,
    q: search.q,
  };
  const key = ["issues", params];
  const list = useInfiniteQuery({
    queryKey: key,
    queryFn: ({ pageParam }) => get<IssuePage>(`/issues${query({ ...params, cursor: pageParam })}`),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next ?? undefined,
    placeholderData: keepPreviousData,
  });

  // A row can move to a later page while the list is read, so it is kept once.
  const rows = useMemo(() => {
    const seen = new Set<number>();
    return (list.data?.pages ?? []).flatMap((p) =>
      p.issues.flatMap((i) => {
        if (seen.has(i.id)) return [];
        seen.add(i.id);
        return [{ ...i, from: p.hours_from }];
      }),
    );
  }, [list.data]);

  const [cursor, setCursor] = useState<number | null>(null);
  const [sel, setSel] = useState<ReadonlySet<number>>(new Set());
  const [leaving, setLeaving] = useState<ReadonlySet<number>>(new Set());
  const scrollTo = useRef(false);
  const searchBox = useRef<HTMLInputElement>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const sentinel = useRef<HTMLTableRowElement>(null);

  const cur = rows.some((r) => r.id === cursor) ? cursor : (rows[0]?.id ?? null);
  const curRow = rows.find((r) => r.id === cur);
  const live = rows.filter((r) => !leaving.has(r.id));
  const chosen = live.filter((r) => sel.has(r.id)).map((r) => r.id);

  const filterKey = JSON.stringify(params);
  useEffect(() => {
    setSel(new Set());
    rememberList(search);
    document.title = "Issues - Davy";
    // The search object is new on every render, its contents change with the key.
  }, [filterKey]);

  const change = useStatusChange((c) => {
    const stays = params.status === undefined || params.status === c.status;
    qc.setQueryData<InfiniteData<IssuePage>>(key, (old) => keepRows(old, (r) => (c.issues.includes(r.id) ? { ...r, status: c.status } : r)));
    if (stays) {
      void qc.invalidateQueries({ queryKey: ["issues"] });
      return;
    }
    const gone = c.issues;
    if (cur !== null && gone.includes(cur)) {
      const at = rows.findIndex((r) => r.id === cur);
      const ok = (r: ListRow) => !gone.includes(r.id) && !leaving.has(r.id);
      const next = rows.slice(at + 1).find(ok) ?? rows.slice(0, at).reverse().find(ok);
      setCursor(next?.id ?? null);
    }
    setLeaving((l) => new Set([...l, ...gone]));
    window.setTimeout(
      () => {
        qc.setQueryData<InfiniteData<IssuePage>>(key, (old) => keepRows(old, (r) => (gone.includes(r.id) ? null : r)));
        setLeaving((l) => without(l, gone));
        void qc.invalidateQueries({ queryKey: ["issues"] });
      },
      reducedMotion() ? 0 : FADE_MS,
    );
  });

  const act = (ids: number[], status: Status) => {
    if (ids.length === 0 || change.isPending) return;
    change.mutate({ issues: ids, status });
    setSel(new Set());
  };

  const open = (id: number | null) => {
    if (id !== null) void navigate({ to: "/issues/$id", params: { id: String(id) } });
  };
  const toggle = (id: number) => setSel((s) => (s.has(id) ? without(s, [id]) : new Set([...s, id])));
  const move = (by: number) => {
    const at = live.findIndex((r) => r.id === cur);
    const next = live[Math.max(0, Math.min(live.length - 1, at + by))];
    if (!next) return;
    scrollTo.current = true;
    setCursor(next.id);
    if (next === live.at(-1) && list.hasNextPage && !list.isFetchingNextPage) void list.fetchNextPage();
  };

  useKeys((e) => {
    const onControl = e.target instanceof Element && e.target.closest("a, button, summary");
    const status = statusKey(e.key);
    if (status) act(chosen.length ? chosen : cur === null ? [] : [cur], status);
    else if (e.key === "j" || e.key === "ArrowDown") move(1);
    else if (e.key === "k" || e.key === "ArrowUp") move(-1);
    else if (e.key === "Enter" && !onControl) open(cur);
    else if (e.key === "x" && cur !== null) toggle(cur);
    else if (e.key === "Escape" && sel.size > 0) setSel(new Set());
    else if (e.key === "/") searchBox.current?.focus();
    else return;
    e.preventDefault();
  });

  useEffect(() => {
    if (!scrollTo.current) return;
    scrollTo.current = false;
    scroller.current?.querySelector(`tr.row[data-id="${cur}"]`)?.scrollIntoView({ block: "nearest" });
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

  const pickSort = (s: Sort) => {
    saveSort(s);
    void navigate({ to: "/issues", search: { ...search, sort: s } });
  };

  const allOn = live.length > 0 && chosen.length === live.length;
  const someOn = chosen.length > 0 && !allOn;

  return (
    <Shell
      keys={KEYS}
      note="Heat strips use one scale for every row"
      rail={(close) => <IssuesRail filters={search} sort={sort} onPick={close} />}
      inspector
    >
      <main id="vlist" aria-label="Issues" aria-busy={list.isFetching}>
        <div className="tb">
          <SearchBox ref={searchBox} search={search} />
          <div className="tbr">
            {change.error ? (
              <span className="problem" role="alert">
                {change.error.message}
              </span>
            ) : null}
            {chosen.length > 0 ? (
              <>
                <span className="cnt">{chosen.length} selected</span>
                <StatusButtons status={null} busy={change.isPending} keys={false} onAct={(s) => act(chosen, s)} />
                <button type="button" className="btn" onClick={() => setSel(new Set())}>
                  Clear
                </button>
              </>
            ) : (
              <span className="cnt flush">
                {live.length} {plural(live.length, "issue")}
                {hasNextPage ? ", more below" : ""}
              </span>
            )}
          </div>
        </div>
        <div className="tscroll" ref={scroller}>
          <table className="iss">
            <colgroup>
              <col className="w-chk" />
              <col className="w-level" />
              <col />
              <col className="w-cul" />
              <col className="w-status" />
              <col className="w-events" />
              <col className="w-age" />
              <col className="w-age" />
              <col className="w-strip" />
            </colgroup>
            <thead>
              <tr>
                <th scope="col" className="chk">
                  <input
                    type="checkbox"
                    aria-label="Select all shown issues"
                    checked={allOn}
                    ref={(el) => {
                      if (el) el.indeterminate = someOn;
                    }}
                    onChange={() => setSel(allOn ? new Set() : new Set(live.map((r) => r.id)))}
                  />
                </th>
                <th scope="col">Level</th>
                <th scope="col">Title</th>
                <th scope="col">Project, culprit</th>
                <th scope="col">Status</th>
                {COLUMNS.map(([s, label]) => (
                  <th key={s} scope="col" className="r" aria-sort={sort === s ? "descending" : undefined}>
                    <button type="button" onClick={() => pickSort(s)}>
                      <span>{label}</span>
                      <span className="ar">{sort === s ? <Icon name="sortd" size={12} /> : null}</span>
                    </button>
                  </th>
                ))}
                <th scope="col" aria-label="Events per hour, last 24 hours, hour of day in UTC">
                  {rows[0] ? <Axis from={rows[0].from} size="sm" /> : null}
                </th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => (
                <Row
                  key={r.id}
                  row={r}
                  now={now}
                  cur={r.id === cur}
                  sel={sel.has(r.id)}
                  leaving={leaving.has(r.id)}
                  onCursor={setCursor}
                  onToggle={toggle}
                  onOpen={open}
                />
              ))}
              {rows.length === 0 ? (
                <tr>
                  <td colSpan={9}>
                    <div className="empty">
                      {list.isPending ? "Loading issues" : list.error ? list.error.message : "No issues match. Clear the search or pick another status."}
                    </div>
                  </td>
                </tr>
              ) : null}
              {hasNextPage ? (
                <tr ref={sentinel} className="more">
                  <td colSpan={9}>
                    <button type="button" className="btn" disabled={isFetchingNextPage} onClick={() => void fetchNextPage()}>
                      {isFetchingNextPage ? "Loading more issues" : "Load more issues"}
                    </button>
                  </td>
                </tr>
              ) : null}
            </tbody>
          </table>
        </div>
      </main>
      <Inspector row={curRow} busy={change.isPending} onAct={(s) => curRow && act([curRow.id], s)} />
    </Shell>
  );
}

type RowProps = {
  row: ListRow;
  now: number;
  cur: boolean;
  sel: boolean;
  leaving: boolean;
  onCursor: (id: number) => void;
  onToggle: (id: number) => void;
  onOpen: (id: number) => void;
};

const Row = memo(function Row({ row, now, cur, sel, leaving, onCursor, onToggle, onOpen }: RowProps) {
  const done = row.status !== "unresolved";
  const cls = ["row", cur && "cur", sel && "sel", leaving && "leaving", done && "done"].filter(Boolean).join(" ");
  const title = shown(row.title);
  const culprit = row.culprit === null ? null : shown(row.culprit);
  return (
    <tr
      className={cls}
      data-id={row.id}
      aria-current={cur ? "true" : undefined}
      onClick={(e) => {
        if (!(e.target instanceof Element && e.target.closest(".cell"))) onCursor(row.id);
      }}
      onDoubleClick={(e) => {
        if (!(e.target instanceof Element && e.target.closest("input"))) onOpen(row.id);
      }}
    >
      <td className="chk">
        <input type="checkbox" checked={sel} aria-label={`Select issue ${row.id}`} onChange={() => onToggle(row.id)} />
      </td>
      <td>
        <Level level={row.level} />
      </td>
      <td className="title">
        <div className="tt">
          <Link to="/issues/$id" params={{ id: String(row.id) }} title={title}>
            {title}
          </Link>
        </div>
      </td>
      <td className="cul" title={culprit ? `${shown(row.project)} / ${culprit}` : shown(row.project)}>
        <span className="pj">{shown(row.project)}</span>
        {culprit ? <span className="fn">{culprit}</span> : <span className="dim">no culprit</span>}
      </td>
      <td>
        <StatusMark status={row.status} />
      </td>
      <td className="r">{row.count.toLocaleString("en-US")}</td>
      <td className="r rel" title={stamp(row.first_seen)}>
        {ago(row.first_seen, now)}
      </td>
      <td className="r rel" title={stamp(row.last_seen)}>
        {ago(row.last_seen, now)}
      </td>
      <td>
        <Strip hourly={row.hourly} from={row.from} size="sm" />
      </td>
    </tr>
  );
});

/** Search as you type, written to the URL a quarter second after the last key. */
function SearchBox({ search, ref }: { search: IssuesSearch; ref: Ref<HTMLInputElement> }) {
  const navigate = useNavigate();
  const [text, setText] = useState(search.q ?? "");
  const typed = useRef(false);
  useEffect(() => {
    if (!typed.current) setText(search.q ?? "");
  }, [search.q]);
  useEffect(() => {
    if (!typed.current) return;
    const t = window.setTimeout(() => {
      typed.current = false;
      const q = text.trim() || undefined;
      if (q !== search.q) void navigate({ to: "/issues", search: { ...search, q }, replace: true });
    }, 250);
    return () => window.clearTimeout(t);
  }, [text, search, navigate]);
  return (
    <div className="search">
      <input
        ref={ref}
        type="search"
        value={text}
        maxLength={200}
        placeholder="Search title and culprit"
        aria-label="Search title and culprit"
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
