import { useInfiniteQuery } from "@tanstack/react-query";
import { Link, getRouteApi, useNavigate } from "@tanstack/react-router";
import { useEffect } from "react";
import { get, query } from "../api/client";
import type { ActorKind, LedgerPage, LedgerRow } from "../api/types";
import { shown } from "../lib/text";
import { stamp } from "../lib/time";
import { RailButton, Shell, type KeyHint } from "./Shell";

const route = getRouteApi("/ledger");

const KEYS: KeyHint[] = [];

/** Who an actor is, next to its name. */
const KINDS: Record<ActorKind, string> = { admin: "admin token", token: "agent token", person: "person", cli: "davy CLI" };

export function Ledger() {
  const { actor } = route.useSearch();
  const navigate = useNavigate();
  const ledger = useInfiniteQuery({
    queryKey: ["ledger", actor ?? null],
    queryFn: ({ pageParam }) => get<LedgerPage>(`/ledger${query({ actor, before: pageParam })}`),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (page) => (page.next_before === null ? undefined : String(page.next_before)),
    refetchInterval: 30_000,
  });
  const pages = ledger.data?.pages ?? [];
  const rows = pages.flatMap((p) => p.rows);
  const actors = pages[0]?.actors ?? [];

  useEffect(() => {
    document.title = "Ledger - Davy";
  }, []);

  const pick = (a: string | undefined, close: () => void) => {
    close();
    void navigate({ to: "/ledger", search: a === undefined ? {} : { actor: a } });
  };
  const rail = (close: () => void) => (
    <div className="rgroup">
      <h2>Actors</h2>
      <RailButton label="Everyone" on={actor === undefined} onClick={() => pick(undefined, close)} />
      {actors.map((a) => (
        <RailButton key={a} label={a} on={actor === a} onClick={() => pick(a, close)} />
      ))}
    </div>
  );

  return (
    <Shell keys={KEYS} rail={rail} inspector={false}>
      <main className="page" aria-label="Ledger" aria-busy={ledger.isFetching}>
        <h1 className="ph">Ledger</h1>
        <p className="lede">
          What agents and people changed, newest first: every MCP call that changed something or that a token's level refused, and a person's changes to issues, retention and{" "}
          <Link to="/tokens">agent tokens</Link>. Reads leave no row. Rows are kept 180 days; each agent token and the admin token keep at most their newest 5,000 changes and 500 refused calls, and a change that deleted data stays the full 180 days. The client is the name an MCP client gave itself, which nothing verifies: the token says who it is.
        </p>
        <section className="sec" aria-labelledby="rows">
          <h3 id="rows">
            Changes<span>{actor ? `by ${shown(actor)}` : "newest first"}</span>
          </h3>
          {ledger.error ? (
            <p className="none">{ledger.error.message}</p>
          ) : ledger.isPending ? (
            <p className="none">Loading the ledger</p>
          ) : rows.length === 0 ? (
            <p className="none">{actor ? `${shown(actor)} changed nothing in the last 180 days.` : "Nothing was changed yet. A resolve, a retention change or an agent's change leaves a row."}</p>
          ) : (
            <>
              <Rows rows={rows} />
              {ledger.hasNextPage ? (
                <div className="acts">
                  <button type="button" className="btn" disabled={ledger.isFetchingNextPage} onClick={() => void ledger.fetchNextPage()}>
                    {ledger.isFetchingNextPage ? "Loading older changes" : "Load older changes"}
                  </button>
                </div>
              ) : null}
            </>
          )}
        </section>
      </main>
    </Shell>
  );
}

function Rows({ rows }: { rows: LedgerRow[] }) {
  return (
    <div className="tscroll">
      <table className="iss plain">
        <colgroup>
          <col className="w-when" />
          <col className="w-actor" />
          <col className="w-action" />
          <col className="w-target" />
          <col className="w-result" />
          <col />
        </colgroup>
        <thead>
          <tr>
            <th scope="col">When</th>
            <th scope="col">Actor</th>
            <th scope="col">Action</th>
            <th scope="col">Target</th>
            <th scope="col">Outcome</th>
            <th scope="col">Summary</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r) => (
            <tr key={r.id}>
              <td className="mono">{stamp(r.at)}</td>
              <td title={r.client ? `${KINDS[r.actor_kind]}; the MCP client called itself ${shown(r.client)}, which nothing verifies` : KINDS[r.actor_kind]}>
                <Link to="/ledger" search={{ actor: r.actor }}>
                  {shown(r.actor)}
                </Link>
                {r.client ? <span className="dim"> client says {shown(r.client)}</span> : null}
              </td>
              <td className="fn">{r.action}</td>
              <td>
                <Target row={r} />
              </td>
              <td>{r.outcome === "done" ? "done" : <span className="problem">{r.outcome}</span>}</td>
              <td title={r.repeats > 1 ? `${r.repeats} times: ${shown(r.summary)}` : shown(r.summary)}>
                {r.repeats > 1 ? <span className="dim">{r.repeats} times: </span> : null}
                {shown(r.summary)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

/** The issue or project a row is about, as a link, else its target. */
function Target({ row: r }: { row: LedgerRow }) {
  const project = r.project === null ? null : shown(r.project);
  if (r.issue !== null) {
    return (
      <>
        <Link to="/issues/$id" params={{ id: String(r.issue) }}>
          issue {r.issue}
        </Link>
        {project ? <span className="dim"> in {project}</span> : null}
      </>
    );
  }
  const target = r.target === null ? null : shown(r.target);
  if (r.project !== null) {
    return (
      <>
        <Link to="/projects" search={{ project: r.project }}>
          {project}
        </Link>
        {target ? <span className="dim">, {target}</span> : null}
      </>
    );
  }
  return target ? <>{target}</> : <span className="dim">none</span>;
}
