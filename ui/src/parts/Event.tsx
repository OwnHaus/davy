// The parts of one event. Every string in an event came from an SDK, so each
// passes through `shown` before React renders it as text.

import { useState } from "react";
import type { Breadcrumb, ColdCut, Context, Cut, EventView, ExceptionView, FrameView, JsonValue, LogLine, Pair } from "../api/types";
import { cutText } from "../lib/cold";
import { announce, copy } from "../lib/live";
import { plural, shown, webUrl } from "../lib/text";
import { clock } from "../lib/time";
import { Icon } from "./Icon";
import { Level } from "./Marks";

/** The frame nearest the crash that is the app's own, else the nearest. */
export function topFrame(ev: EventView): FrameView | null {
  const frames = ev.exceptions.items.at(-1)?.frames.items ?? [];
  return frames.findLast((f) => f.in_app) ?? frames.at(-1) ?? null;
}

/** `file:line:column`, with what the frame has of it. */
export function where(f: FrameView): string {
  const file = f.filename ?? f.abs_path ?? f.module ?? "unknown file";
  return shown(`${file}${f.lineno === null ? "" : `:${f.lineno}`}${f.colno === null ? "" : `:${f.colno}`}`);
}

export function CopyButton({ text, label }: { text: string; label: string }) {
  const [done, setDone] = useState(false);
  return (
    <button
      type="button"
      className="cp"
      title={`Copy ${label}`}
      aria-label={`Copy ${label}`}
      onClick={async () => {
        const ok = await copy(text);
        announce(ok ? `${label} copied` : "Could not copy");
        if (!ok) return;
        setDone(true);
        window.setTimeout(() => setDone(false), 1600);
      }}
    >
      <Icon name={done ? "check" : "copy"} />
    </button>
  );
}

/** `N more not shown`, when Davy cut a long list. */
export function More({ cut, what }: { cut: Cut<unknown>; what: string }) {
  if (cut.more === 0) return null;
  return (
    <div className="more-note">
      {cut.more} more {plural(cut.more, what)} not shown
    </div>
  );
}

export function Exceptions({ ev, eventKey }: { ev: EventView; eventKey: string }) {
  if (ev.exceptions.items.length === 0) {
    return (
      <section className="sec">
        <h3>Message</h3>
        <div className="exc">
          <span className="va">{ev.message === null ? "This event has neither an exception nor a message." : shown(ev.message)}</span>
        </div>
      </section>
    );
  }
  // The SDK sends the exception thrown last at the end; it reads best first.
  const thrown = [...ev.exceptions.items].reverse();
  return (
    <>
      {thrown.map((x, n) => (
        <Exception key={`${eventKey}:${n}`} x={x} />
      ))}
      <More cut={ev.exceptions} what="exception" />
    </>
  );
}

function Exception({ x }: { x: ExceptionView }) {
  const frames = [...x.frames.items].reverse();
  const [open, setOpen] = useState<boolean[]>(() => frames.map((f) => f.in_app));
  const head = [x.type, x.value].filter((s) => s !== null).map((s) => shown(s));
  return (
    <section className="sec">
      <h3>
        Exception<span>{frames.length > 0 ? "crash-nearest frame first" : "no stack trace"}</span>
      </h3>
      <div className="exc">
        <div>
          {head.length === 2 ? (
            <>
              <span className="ty">{head[0]}: </span>
              <span className="va">{head[1]}</span>
            </>
          ) : (
            <span className="ty">{head[0] ?? "Unnamed exception"}</span>
          )}
        </div>
        {x.mechanism !== null || x.handled !== null ? (
          <div className="mech">
            {[x.mechanism === null ? null : `mechanism ${shown(x.mechanism)}`, x.handled === null ? null : x.handled ? "handled" : "unhandled"]
              .filter(Boolean)
              .join(", ")}
          </div>
        ) : null}
      </div>
      {frames.map((f, k) => (
        <Frame key={k} f={f} open={open[k] ?? false} onToggle={() => setOpen((o) => o.map((v, i) => (i === k ? !v : v)))} />
      ))}
      <More cut={x.frames} what="outer frame" />
    </section>
  );
}

function Frame({ f, open, onToggle }: { f: FrameView; open: boolean; onToggle: () => void }) {
  const lines = f.context_line !== null;
  const first = (f.lineno ?? f.pre_context.length + 1) - f.pre_context.length;
  return (
    <div className={`frame${f.in_app ? " mine" : ""}${open ? " open" : ""}`}>
      <button type="button" className="fbtn" aria-expanded={open} onClick={onToggle}>
        <span className="chev">
          <Icon name="chev" size={12} />
        </span>
        <span className="fnm">{f.function === null ? "anonymous" : shown(f.function)}</span>
        <span className="loc">{where(f)}</span>
        <span className="fl">
          {f.minified !== null ? (
            <span className="inapp" title="Position mapped through the uploaded source map">
              <Icon name="smap" />
              source map
            </span>
          ) : null}
          {f.in_app ? <span className="inapp">in app</span> : <span>library</span>}
        </span>
      </button>
      <div className="fold">
        <div>
          {f.minified !== null ? (
            <div className="org2">
              Original position, mapped from<span className="mono">{shown(f.minified)}</span>
            </div>
          ) : null}
          {lines ? (
            <div className="code" role="group" aria-label={`Source around line ${f.lineno ?? first + f.pre_context.length}`}>
              {f.pre_context.map((t, k) => (
                <Line key={`p${k}`} no={first + k} text={t} />
              ))}
              <Line no={first + f.pre_context.length} text={f.context_line ?? ""} hit />
              {f.post_context.map((t, k) => (
                <Line key={`n${k}`} no={first + f.pre_context.length + 1 + k} text={t} />
              ))}
            </div>
          ) : (
            <dl className="kv fmeta">
              {f.module !== null ? (
                <>
                  <dt>Module</dt>
                  <dd className="mono">{shown(f.module)}</dd>
                </>
              ) : null}
              <dt>File</dt>
              <dd className="mono">{shown(f.filename ?? f.abs_path ?? "unknown")}</dd>
              {f.lineno !== null ? (
                <>
                  <dt>Position</dt>
                  <dd className="mono">
                    line {f.lineno}
                    {f.colno === null ? "" : `, column ${f.colno}`}
                  </dd>
                </>
              ) : null}
              <dt>Context</dt>
              <dd className="none">{f.in_app ? "The SDK sent no source lines for this frame." : "No source lines for library frames."}</dd>
            </dl>
          )}
        </div>
      </div>
    </div>
  );
}

function Line({ no, text, hit }: { no: number; text: string; hit?: boolean }) {
  return (
    <div className={hit ? "ln hit" : "ln"} aria-label={hit ? `Crash line ${no}` : undefined}>
      <span className="no">{no}</span>
      <span className="tx">{text === "" ? " " : shown(text)}</span>
    </div>
  );
}

export function Breadcrumbs({ cut }: { cut: Cut<Breadcrumb> }) {
  return (
    <section className="sec">
      <h3>
        Breadcrumbs<span>{cut.items.length + cut.more}</span>
      </h3>
      {cut.items.length === 0 ? (
        <div className="none">No breadcrumbs on this event.</div>
      ) : (
        <div className="rowlist">
          {cut.more > 0 ? <div className="more-note">{cut.more} older not shown</div> : null}
          {cut.items.map((b, k) => (
            <div key={k} className="rl bc">
              <span className="t">{b.timestamp === null ? "" : clock(b.timestamp)}</span>
              <span className="c">{shown(b.category ?? b.type ?? "")}</span>
              <span>{b.level === null ? null : <Level level={b.level} />}</span>
              <span className="m">
                {b.message === null ? null : shown(b.message)}
                {b.data.items.length > 0 ? <span className="at">{pairsText(b.data.items)}</span> : null}
              </span>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

const pairsText = (pairs: Pair[]) => pairs.map((p) => `${shown(p.key)}=${shown(p.value)}`).join("  ");

const jsonText = (v: JsonValue) => (typeof v === "string" ? v : JSON.stringify(v));

export function LogLines({ scope, logs, cut }: { scope: string; logs: LogLine[]; cut: ColdCut | null }) {
  const stopped = cutText(cut, "The search");
  return (
    <section className="sec">
      <h3>
        Logs around the event<span>{shown(scope)}</span>
      </h3>
      {stopped ? <div className="none">{stopped}</div> : null}
      {logs.length === 0 ? (
        <div className="none">No logs in that window.</div>
      ) : (
        <div className="rowlist">
          {logs.map((l, k) => {
            const attrs = Object.entries(l.attributes);
            return (
              <div key={k} className="rl lg">
                <span className="t">{clock(l.timestamp)}</span>
                <Level level={l.level} />
                <span className="c">{l.service === null ? l.source : shown(l.service)}</span>
                <span className="m">
                  {shown(l.message)}
                  {attrs.length > 0 ? <span className="at">{attrs.map(([key, v]) => `${shown(key)}=${shown(jsonText(v))}`).join("  ")}</span> : null}
                </span>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}

export function Pairs({ cut, empty }: { cut: Cut<Pair>; empty: string }) {
  if (cut.items.length === 0) return <div className="none">{empty}</div>;
  return (
    <>
      <dl className="kv tonal">
        {cut.items.map((p, k) => (
          <Kv key={k} label={shown(p.key)} value={shown(p.value)} />
        ))}
      </dl>
      <More cut={cut} what="value" />
    </>
  );
}

function Kv({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </>
  );
}

export function Contexts({ cut }: { cut: Cut<Context> }) {
  return (
    <section className="sec">
      <h3>
        Contexts<span>{cut.items.length + cut.more}</span>
      </h3>
      {cut.items.length === 0 ? <div className="none">No contexts on this event.</div> : null}
      {cut.items.map((c) => (
        <div key={c.name} className="ctx">
          <h4>{shown(c.name)}</h4>
          <Pairs cut={c.fields} empty="No fields." />
        </div>
      ))}
      <More cut={cut} what="context" />
    </section>
  );
}

export function UserAndTrace({ ev }: { ev: EventView }) {
  const url = ev.request?.url ?? null;
  const href = url === null ? null : webUrl(url);
  return (
    <section className="sec">
      <h3>User and trace</h3>
      <Pairs cut={ev.user} empty="No user on this event." />
      <dl className="kv tonal gap">
        <dt>Trace id</dt>
        <dd>
          {ev.trace_id === null ? (
            <span className="none">not set</span>
          ) : (
            <>
              <span className="mono">{shown(ev.trace_id)}</span>
              <CopyButton text={ev.trace_id} label="trace id" />
            </>
          )}
        </dd>
        <dt>Request</dt>
        <dd>
          {ev.request === null ? (
            <span className="none">none on this event</span>
          ) : (
            <span className="mono">
              {ev.request.method === null ? "" : `${shown(ev.request.method)} `}
              {url === null ? null : href === null ? (
                shown(url)
              ) : (
                <a href={href} rel="noreferrer noopener" target="_blank">
                  {shown(url)}
                </a>
              )}
            </span>
          )}
        </dd>
      </dl>
    </section>
  );
}
