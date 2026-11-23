import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useEffect, useState, type ReactNode } from "react";
import { get, post } from "../api/client";
import type { Me, Project, Sort } from "../api/types";
import { liveRegion } from "../lib/live";
import { Legend, Tip } from "../parts/HeatStrip";
import { Brand, Icon } from "../parts/Icon";
import { lastList, saveSort, type IssuesSearch, type StatusFilter } from "../search";

/** A key and what it does, for the footer. */
export type KeyHint = [string, string | null, string];

type ShellProps = {
  crumb?: ReactNode;
  keys: KeyHint[];
  note?: string;
  /** The filters in the rail. `close` hides the rail where it is an overlay. */
  rail: (close: () => void) => ReactNode;
  /** Whether the page has its own inspector column. */
  inspector: boolean;
  children: ReactNode;
};

export function Shell({ crumb, keys, note, rail, inspector, children }: ShellProps) {
  const [railOpen, setRailOpen] = useState(false);
  return (
    <div className="app">
      <header className="top">
        <button
          type="button"
          className="railbtn cp"
          aria-label="Filters"
          aria-expanded={railOpen}
          aria-controls="rail"
          onClick={() => setRailOpen((o) => !o)}
        >
          <Icon name="filters" size={14} />
        </button>
        <Link className="brand" to="/issues" aria-label="Davy, issues">
          <Brand />
          <span>Davy</span>
        </Link>
        <nav className="views" aria-label="Views">
          <Link to="/issues" search={lastList} data-text="Issues">
            Issues
          </Link>
          <Link to="/logs" data-text="Logs">
            Logs
          </Link>
        </nav>
        {crumb ? (
          <nav className="crumb" aria-label="Location">
            {crumb}
          </nav>
        ) : null}
        <Account />
      </header>
      <div className={`body${inspector ? "" : " wide"}`}>
        <aside id="rail" className={railOpen ? "open" : undefined} aria-label="Filters">
          {rail(() => setRailOpen(false))}
        </aside>
        {children}
      </div>
      <footer className="foot">
        {keys.map(([a, b, what]) => (
          <span key={what}>
            <kbd>{a}</kbd>
            {b ? <kbd>{b}</kbd> : null}
            {what}
          </span>
        ))}
        {note ? <span className="sp">{note}</span> : null}
      </footer>
      <Tip />
      <div className="sr-only" role="status" aria-live="polite" ref={liveRegion} />
    </div>
  );
}

function Account() {
  const me = useQuery({ queryKey: ["me"], queryFn: () => get<Me>("/me"), staleTime: Infinity });
  const out = async () => {
    await post("/logout", {}).catch(() => undefined);
    window.location.replace("/ui/login");
  };
  return (
    <div className="acct">
      <span className="who">{me.data?.email}</span>
      <button type="button" className="btn" onClick={out}>
        Log out
      </button>
    </div>
  );
}

const STATUS_PICKS: [StatusFilter, string][] = [
  ["unresolved", "Unresolved"],
  ["resolved", "Resolved"],
  ["ignored", "Ignored"],
  ["all", "All"],
];

const SORT_PICKS: [Sort, string][] = [
  ["last_seen", "Last seen"],
  ["events", "Events"],
  ["first_seen", "First seen"],
];

/** The issue filters, for the issue list and an issue's page. */
export function IssuesRail({ filters, sort, onPick }: { filters: IssuesSearch; sort: Sort; onPick: () => void }) {
  const navigate = useNavigate();
  const projects = useQuery({ queryKey: ["projects"], queryFn: () => get<Project[]>("/projects"), staleTime: 60_000 });
  const pick = (change: IssuesSearch) => {
    onPick();
    void navigate({ to: "/issues", search: { ...filters, ...change } });
  };
  const status = filters.status ?? "unresolved";
  const releases = (projects.data ?? []).filter((p) => !filters.project || p.slug === filters.project).flatMap((p) => p.releases);
  return (
    <>
      <div className="rgroup">
        <h2>Projects</h2>
        <RailButton label="All projects" on={!filters.project} onClick={() => pick({ project: undefined, release: undefined })} />
        {projects.data?.map((p) => (
          <RailButton key={p.slug} label={p.slug} on={filters.project === p.slug} onClick={() => pick({ project: p.slug, release: undefined })} />
        ))}
      </div>
      <div className="rgroup">
        <h2>Status</h2>
        {STATUS_PICKS.map(([value, label]) => (
          <RailButton key={value} label={label} on={status === value} onClick={() => pick({ status: value === "unresolved" ? undefined : value })} />
        ))}
      </div>
      <div className="rgroup">
        <h2>
          <label htmlFor="env">Environment</label>
        </h2>
        <Typed id="env" value={filters.environment} placeholder="Any environment" onSet={(environment) => pick({ environment })} />
      </div>
      <div className="rgroup">
        <h2>
          <label htmlFor="rel">Release</label>
        </h2>
        <Typed id="rel" value={filters.release} placeholder="Any release" list={releases} onSet={(release) => pick({ release })} />
      </div>
      <div className="rgroup">
        <h2>Sort</h2>
        {SORT_PICKS.map(([value, label]) => (
          <RailButton
            key={value}
            label={label}
            on={sort === value}
            onClick={() => {
              saveSort(value);
              pick({ sort: value });
            }}
            extra={sort === value ? <span className="dir"><Icon name="sortd" /></span> : <span />}
          />
        ))}
        <div className="rnote">Saved on this device</div>
      </div>
      <Legend />
    </>
  );
}

export function RailButton({ label, on, onClick, extra }: { label: string; on: boolean; onClick: () => void; extra?: ReactNode }) {
  return (
    <button type="button" className="rb" aria-pressed={on} onClick={onClick}>
      <span className="l">{label}</span>
      {extra ?? <span className="n" />}
    </button>
  );
}

/** A filter typed in full, applied on Enter or when the field loses focus. */
function Typed({ id, value, placeholder, list, onSet }: { id: string; value: string | undefined; placeholder: string; list?: string[]; onSet: (v: string | undefined) => void }) {
  const [text, setText] = useState(value ?? "");
  useEffect(() => setText(value ?? ""), [value]);
  const apply = () => {
    const v = text.trim() || undefined;
    if (v !== value) onSet(v);
  };
  return (
    <div className="rin">
      <input
        id={id}
        value={text}
        placeholder={placeholder}
        autoComplete="off"
        spellCheck={false}
        maxLength={200}
        list={list?.length ? `${id}-list` : undefined}
        onChange={(e) => setText(e.target.value)}
        onBlur={apply}
        onKeyDown={(e) => {
          if (e.key === "Enter") apply();
          if (e.key === "Escape") {
            e.stopPropagation();
            setText(value ?? "");
            e.currentTarget.blur();
          }
        }}
      />
      {list?.length ? (
        <datalist id={`${id}-list`}>
          {list.map((r) => (
            <option key={r} value={r} />
          ))}
        </datalist>
      ) : null}
    </div>
  );
}

export function NotFound() {
  useEffect(() => {
    document.title = "Not found - Davy";
  }, []);
  return (
    <main className="lone">
      <h1>There is no such page</h1>
      <p>
        <Link to="/issues">Go to the issues</Link>
      </p>
    </main>
  );
}
