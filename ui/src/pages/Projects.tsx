import { useQuery } from "@tanstack/react-query";
import { getRouteApi, useNavigate } from "@tanstack/react-router";
import { useEffect, useRef, type KeyboardEvent } from "react";
import { get } from "../api/client";
import type { Endpoints, ProjectKey } from "../api/types";
import { shown } from "../lib/text";
import { CopyButton } from "../parts/Event";
import { SDKS, SDK_NAMES, guide, type Sdk } from "../parts/snippets";
import { RailButton, Shell, type KeyHint } from "./Shell";

const route = getRouteApi("/projects");

const KEYS: KeyHint[] = [["←", "→", "SDK tabs"]];

export const connectQuery = { queryKey: ["connect"], queryFn: () => get<Endpoints>("/connect"), staleTime: 60_000 };

export function Projects() {
  const search = route.useSearch();
  const navigate = useNavigate();
  const connect = useQuery(connectQuery);
  const projects = connect.data?.projects ?? [];
  const picked = projects.find((p) => p.slug === search.project) ?? projects[0];
  const sdk = search.sdk ?? "node";

  useEffect(() => {
    document.title = "Projects - Davy";
  }, []);

  const rail = (close: () => void) => (
    <div className="rgroup">
      <h2>Projects</h2>
      {projects.map((p) => (
        <RailButton
          key={p.slug}
          label={p.slug}
          on={picked?.slug === p.slug}
          onClick={() => {
            close();
            void navigate({ to: "/projects", search: { project: p.slug, sdk: search.sdk }, replace: true });
          }}
        />
      ))}
    </div>
  );

  return (
    <Shell keys={KEYS} rail={!connect.data || projects.length ? rail : undefined} inspector={false}>
      <main className="page" aria-label="Projects" aria-busy={connect.isFetching}>
        <div className="col">
          <h1 className="ph">Projects</h1>
          {connect.error ? (
            <p className="none">{connect.error.message}</p>
          ) : !connect.data ? (
            <p className="none">Loading projects</p>
          ) : (
            <>
              <Endpoint data={connect.data} />
              {picked ? (
                <Project project={picked} data={connect.data} sdk={sdk} />
              ) : (
                <section className="sec" aria-labelledby="noproj">
                  <h3 id="noproj">No projects yet</h3>
                  <p className="none">A project holds the errors and logs of one app. Create one on the server. It prints the DSN to give the SDK.</p>
                  <pre className="cmd">davy project create my-app --data /var/lib/davy</pre>
                </section>
              )}
            </>
          )}
        </div>
      </main>
    </Shell>
  );
}

function Endpoint({ data }: { data: Endpoints }) {
  const rows: [string, string][] = [
    ["Public URL", data.public_url],
    ["Logs", data.logs],
    ["OpenTelemetry", data.otlp],
    ["MCP", data.mcp],
  ];
  return (
    <section className="sec" aria-labelledby="ends">
      <h3 id="ends">
        This server<span>what clients send to</span>
      </h3>
      <dl className="kv tonal copyable">
        {rows.map(([label, value]) => (
          <Copyable key={label} label={label} value={value} />
        ))}
      </dl>
    </section>
  );
}

function Copyable({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>
        <span className="mono">{shown(value)}</span>
        <CopyButton text={value} label={label} />
      </dd>
    </>
  );
}

function Project({ project, data, sdk }: { project: ProjectKey; data: Endpoints; sdk: Sdk }) {
  const navigate = useNavigate();
  const tabs = useRef<HTMLDivElement>(null);
  const fill = {
    url: data.public_url.replace(/\/+$/, ""),
    logs: data.logs,
    otlp: data.otlp,
    slug: project.slug,
    dsn: project.dsn,
    key: project.key,
  };
  const g = guide(sdk, fill);

  const pick = (next: Sdk, focus: boolean) => {
    void navigate({ to: "/projects", search: { project: project.slug, sdk: next }, replace: true });
    if (focus) tabs.current?.querySelector<HTMLButtonElement>(`#tab-${next}`)?.focus();
  };
  const onKey = (e: KeyboardEvent) => {
    const at = SDKS.indexOf(sdk);
    const to = { ArrowLeft: at - 1, ArrowRight: at + 1, Home: 0, End: SDKS.length - 1 }[e.key];
    if (to === undefined) return;
    e.preventDefault();
    const next = SDKS[(to + SDKS.length) % SDKS.length];
    if (next) pick(next, true);
  };

  return (
    <>
      <section className="sec" aria-labelledby="keys">
        <h3 id="keys">
          {shown(project.slug)}
          <span>the DSN lets a client send errors, the key lets it send logs</span>
        </h3>
        <dl className="kv tonal copyable">
          <Copyable label="DSN" value={project.dsn} />
          <Copyable label="Key" value={project.key} />
        </dl>
      </section>
      <section className="sec" aria-labelledby="send">
        <h3 id="send">Send from your app</h3>
        <div className="tabs" role="tablist" aria-label="SDK" ref={tabs} onKeyDown={onKey}>
          {SDKS.map((s) => (
            <button
              key={s}
              id={`tab-${s}`}
              type="button"
              role="tab"
              aria-selected={s === sdk}
              aria-controls="sdk-panel"
              tabIndex={s === sdk ? 0 : -1}
              data-text={SDK_NAMES[s]}
              onClick={() => pick(s, false)}
            >
              {SDK_NAMES[s]}
            </button>
          ))}
        </div>
        <div id="sdk-panel" role="tabpanel" aria-labelledby={`tab-${sdk}`} className="tabpanel">
          <p>{g.intro}</p>
          {g.blocks.map((b) => (
            <div key={b.label} className="snip">
              <div className="sh">
                <span>{b.label}</span>
                <CopyButton text={b.code} label={b.label} />
              </div>
              <pre>
                <code>{b.code}</code>
              </pre>
            </div>
          ))}
          <p className="hint">
            More in <span className="mono">{g.doc}</span> in the Davy repository.
          </p>
        </div>
      </section>
    </>
  );
}
