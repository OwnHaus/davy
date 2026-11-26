// The savings view of `davy usage`: the log patterns that take the most
// room, retention longer than anyone reads, the noisiest issues and the drop
// rules on trial. The server says each rule and command, the page shows them
// and runs none: a person copies a command and runs it on the server.

import { Link } from "@tanstack/react-router";
import type { ReactNode } from "react";
import type { IssueEntry, PatternEntry, RetentionEntry, Savings as SavingsData, ShadowEntry } from "../api/usage";
import { agoLong, stamp } from "../lib/time";
import { bytes, plural, shown } from "../lib/text";
import { CopyButton } from "./Event";

const percent = (share: number) => `${Math.round(share * 100)}%`;
const days = (n: number) => `${n} ${plural(n, "day")}`;
const count = (n: number) => n.toLocaleString("en-US");
const capital = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

/**
 * A command a person runs on the server. The text is shown, never run. A
 * command holding a character `shown()` escapes gets no copy button, as
 * what it would copy is not what the server would run.
 */
function Command({ text, label }: { text: string; label: string }) {
  const safe = shown(text);
  return (
    <div className="snip">
      <div className="sh">
        <span>{label}</span>
        {safe === text ? <CopyButton text={text} label={label} /> : null}
      </div>
      <pre>
        <code>{safe}</code>
      </pre>
      {safe === text ? null : <p className="snote">It holds characters that do not print, shown as escapes, so it has no copy button.</p>}
    </div>
  );
}

/** What an agent asks over MCP to look closer. */
function Try({ text }: { text: string }) {
  return (
    <p className="try">
      <span>Agents look closer with</span> <code>{shown(text)}</code>
      {shown(text) === text ? <CopyButton text={text} label="agent query" /> : null}
    </p>
  );
}

function Pattern({ p }: { p: PatternEntry }) {
  const of = [p.project, p.service, p.level].filter((s): s is string => s !== null).map(shown).join(" ");
  return (
    <li className="sv">
      <div className="svhead">
        <span className="share" aria-hidden="true">
          <i style={{ width: `${Math.max(2, Math.round(p.share * 100))}%` }} />
        </span>
        <b>{percent(p.share)}</b>
        <span>of log bytes</span>
        <span>{bytes(p.per_day)}/day</span>
        <span title={p.last_queried ? stamp(p.last_queried) : undefined}>last queried {p.last_queried ? agoLong(p.last_queried) : "never"}</span>
      </div>
      <pre className="example">{shown(p.example)}</pre>
      <p className="svmeta">
        <span className="mono">{p.id}</span>
        {of ? <span>{of}</span> : null}
        <span>{bytes(p.bytes)} in the window</span>
      </p>
      {p.rule ? (
        <p>
          Rule: {shown(p.rule)}
          {p.saves_per_day !== null ? `, saves ${bytes(p.saves_per_day)}/day` : ""}.
        </p>
      ) : p.suggest ? (
        <p>{capital(shown(p.suggest))}.</p>
      ) : null}
      {p.command ? <Command text={p.command} label="Add the rule on the server" /> : null}
      <Try text={p.try} />
    </li>
  );
}

function Retention({ r }: { r: RetentionEntry }) {
  return (
    <li className="sv">
      <p>
        <b>{shown(r.project)}</b> keeps {days(r.logs_days)} of logs. {count(r.searches)} {plural(r.searches, "search", "searches")} in {days(r.history_days)}, the
        oldest read {days(r.oldest_read_days)} back. A shorter retention saves {bytes(r.saves)}.
      </p>
      <Command text={r.command} label="Shorten it on the server" />
      <Try text={r.try} />
    </li>
  );
}

function Issue({ i }: { i: IssueEntry }) {
  return (
    <li className="sv">
      <p>
        <Link className="ilink" to="/issues/$id" params={{ id: String(i.id) }}>
          {shown(i.title)}
        </Link>
      </p>
      <p className="svmeta">
        <span>{shown(i.project)}</span>
        <span>
          {count(i.events)} {plural(i.events, "event")}, {percent(i.share)} of its error events
        </span>
      </p>
      <p>Fix it, or discard it. Discarding also stops storing its real events.</p>
      <Command text={i.command} label="Discard it on the server" />
      <Try text={i.try} />
    </li>
  );
}

/** How long a rule on trial has counted and what it would have dropped, in words. */
export function counted(s: Pick<ShadowEntry, "hours" | "would_drop" | "saves_per_day">): string {
  const lines = s.would_drop.records;
  const what = lines === 0 ? "nothing" : `${count(lines)} ${plural(lines, "line")} (${bytes(s.would_drop.bytes)})`;
  if (s.saves_per_day !== null) return `Counting for ${(s.hours / 24).toFixed(1)} days: it would drop ${what}, ${bytes(s.saves_per_day)}/day.`;
  const hours = Math.floor(s.hours);
  const time = hours < 1 ? "less than an hour" : `${hours} ${plural(hours, "hour")}`;
  return `Counting for ${time}: it would have dropped ${what} so far. A rate per day follows after a full day.`;
}

function Shadow({ s }: { s: ShadowEntry }) {
  return (
    <li className="sv">
      <p>
        <b>Rule {s.id}</b> in {shown(s.project)} by {s.by}: {shown(s.rule)}
      </p>
      <p>{counted(s)}</p>
      {s.hides ? <p>It would hide {shown(s.hides)}, so it stays on trial.</p> : null}
      {s.command ? <Command text={s.command} label="Make it live on the server" /> : null}
    </li>
  );
}

function Group({ id, title, lead, children }: { id: string; title: string; lead?: string | undefined; children: ReactNode[] }) {
  if (children.length === 0) return null;
  return (
    <section className="svgroup" aria-labelledby={id}>
      <h4 id={id}>{title}</h4>
      {lead ? <p className="svlead">{lead}</p> : null}
      <ul className="svlist">{children}</ul>
    </section>
  );
}

/** The first sentence of the note, for a person, then all of it for agents. */
function Note({ note }: { note: string }) {
  return (
    <>
      <p className="hint">Last queried is approximate, and counts only searches on the logs page and with search_logs or count_logs.</p>
      <details className="svnote">
        <summary>The full note for agents</summary>
        <p>{note}</p>
      </details>
    </>
  );
}

export function Savings({ data, project }: { data: SavingsData; project: string | undefined }) {
  const all = data.savings;
  const patterns = all.filter((e): e is PatternEntry => e.kind === "pattern");
  const retention = all.filter((e): e is RetentionEntry => e.kind === "retention");
  const issues = all.filter((e): e is IssueEntry => e.kind === "issue");
  const shadows = all.filter((e): e is ShadowEntry => e.kind === "shadow");
  return (
    <>
      <p className="lede">Davy changes nothing from this page. Copy a command and run it on the server.</p>
      {all.length === 0 ? <p className="none">Nothing to suggest in this window.</p> : null}
      <Group id="sv-patterns" title="Log patterns" lead={project ? undefined : "Pick a project to get a drop command."}>
        {patterns.map((p) => (
          <Pattern key={`${p.project ?? ""}-${p.id}`} p={p} />
        ))}
      </Group>
      <Group id="sv-retention" title="Retention longer than anyone reads">
        {retention.map((r) => (
          <Retention key={r.project} r={r} />
        ))}
      </Group>
      <Group id="sv-issues" title="Noisy issues">
        {issues.map((i) => (
          <Issue key={i.id} i={i} />
        ))}
      </Group>
      <Group id="sv-rules" title="Drop rules on trial">
        {shadows.map((s) => (
          <Shadow key={s.id} s={s} />
        ))}
      </Group>
      {data.patterns_partial ? <p className="hint">Some days of pattern counters were not read in full.</p> : null}
      <Note note={data.note} />
    </>
  );
}
