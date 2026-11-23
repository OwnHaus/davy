// The rows of the Logs page. Every string in a log came from a sender, so each
// passes through `shown` before React renders it as text.

import { memo, useId } from "react";
import type { JsonValue, LogLine } from "../api/types";
import { shown } from "../lib/text";
import { day, hms, msOf } from "../lib/time";
import { CopyButton } from "./Event";
import { Icon } from "./Icon";
import { Level } from "./Marks";

/** A log with a key that tells it from its neighbours, and whether live just brought it. */
export type LogRow = LogLine & { key: string; fresh: boolean };

/** What tells two logs apart; two logs alike in all of it count as one. */
export const sameness = (l: LogLine) => `${l.timestamp_ns}|${l.project}|${l.level}|${l.service ?? ""}|${l.message}`;

/** Attributes as dotted paths. The path keeps each key, a key may hold dots itself. */
function flat(obj: { [key in string]: JsonValue }, path: string[] = []): [string[], JsonValue][] {
  return Object.entries(obj).flatMap(([k, v]): [string[], JsonValue][] =>
    v !== null && typeof v === "object" && !Array.isArray(v) ? flat(v, [...path, k]) : [[[...path, k], v]],
  );
}

const valueText = (v: JsonValue) => (typeof v === "string" ? v : JSON.stringify(v));
const SIMPLE_KEY = /^[A-Za-z0-9_]+$/;

/**
 * The query that finds logs with this attribute value, or null when the query
 * syntax cannot say it. A dot inside a key is escaped, the path joins keys
 * with plain dots. Text is a phrase, numbers and true or false stand bare.
 */
export function attributeTerm(path: string[], v: JsonValue): string | null {
  if (!path.every((k) => SIMPLE_KEY.test(k.replaceAll(".", "")))) return null;
  const field = `attributes.${path.map((k) => k.replaceAll(".", "\\.")).join(".")}`;
  if (typeof v === "number" || typeof v === "boolean") return `${field}:${v}`;
  if (typeof v !== "string" || v.length === 0 || v.length > 200) return null;
  return `${field}:"${v.replace(/["\\]/g, (c) => `\\${c}`)}"`;
}

type RowProps = {
  row: LogRow;
  cur: boolean;
  open: boolean;
  /** Whether rows say their project, when the page shows more than one. */
  project: boolean;
  /** Whether times say their day, when the range spans more than one. */
  days: boolean;
  onToggle: (key: string) => void;
  onFilter: (term: string) => void;
};

export const LogRowView = memo(function LogRowView({ row, cur, open, project, days, onToggle, onFilter }: RowProps) {
  const ms = msOf(row.timestamp_ns);
  const attrs = flat(row.attributes);
  const cls = ["lrow", cur && "cur", open && "open", row.fresh && "fresh"].filter(Boolean).join(" ");
  const detail = useId();
  return (
    <li className={cls} data-key={row.key}>
      <button type="button" className="lhead" aria-expanded={open} aria-controls={open ? detail : undefined} onClick={() => onToggle(row.key)}>
        <span className="chev" aria-hidden="true">
          <Icon name="chev" size={12} />
        </span>
        <span className="t">
          {days ? `${day(ms)} ` : ""}
          {hms(ms)}
        </span>
        <Level level={row.level} />
        <span className="c">{row.service === null ? row.source : shown(row.service)}</span>
        <span className="m">
          {project ? <span className="pj">{shown(row.project)}</span> : null}
          {shown(row.message)}
          {attrs.length > 0 ? <span className="at">{attrs.map(([p, v]) => `${shown(p.join("."))}=${shown(valueText(v))}`).join("  ")}</span> : null}
        </span>
      </button>
      {open ? <Detail id={detail} row={row} attrs={attrs} onFilter={onFilter} /> : null}
    </li>
  );
});

function Detail({ id, row, attrs, onFilter }: { id: string; row: LogRow; attrs: [string[], JsonValue][]; onFilter: (term: string) => void }) {
  const fields: [string, string | null][] = [
    ["Time", row.timestamp],
    ["Project", row.project],
    ["Source", row.source],
    ["Service", row.service],
    ["Trace", row.trace_id],
    ["Span", row.span_id],
  ];
  return (
    <div className="ldet" id={id}>
      <pre className="lmsg">{shown(row.message)}</pre>
      <dl className="kv tonal lkv">
        {fields.map(([label, value]) =>
          value === null ? null : (
            <Pair key={label} label={label} value={shown(value)} copy={label === "Trace" || label === "Time" ? value : null} term={null} onFilter={onFilter} />
          ),
        )}
        {attrs.map(([path, v]) => {
          const key = path.join(".");
          return <Pair key={`a:${key}`} label={shown(key)} value={shown(valueText(v))} copy={null} term={attributeTerm(path, v)} onFilter={onFilter} />;
        })}
      </dl>
    </div>
  );
}

type PairProps = { label: string; value: string; copy: string | null; term: string | null; onFilter: (term: string) => void };

function Pair({ label, value, copy, term, onFilter }: PairProps) {
  return (
    <>
      <dt>{label}</dt>
      <dd>
        <span className="mono">{value}</span>
        {copy ? <CopyButton text={copy} label={label.toLowerCase()} /> : null}
        {term ? (
          <button type="button" className="cp" title="Search for this value" aria-label={`Search for ${label} ${value}`} onClick={() => onFilter(term)}>
            <Icon name="filters" size={12} />
          </button>
        ) : null}
      </dd>
    </>
  );
}
