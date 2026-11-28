// The state of every view lives in the URL, so a link shows what its sender saw.

import type { LogLevel, Sort, Status } from "./api/types";
import { SDKS, type Sdk } from "./parts/snippets";

export type StatusFilter = Status | "all";

export type IssuesSearch = {
  project?: string | undefined;
  status?: StatusFilter | undefined;
  environment?: string | undefined;
  release?: string | undefined;
  q?: string | undefined;
  sort?: Sort | undefined;
};

export type IssueSearch = { event?: string | undefined };

const text = (v: unknown) => (typeof v === "string" && v !== "" ? v : undefined);
const oneOf = <T extends string>(v: unknown, allowed: readonly T[]) => allowed.find((a) => a === v);

export const SORTS = ["last_seen", "events", "first_seen"] as const satisfies readonly Sort[];
const STATUSES = ["unresolved", "resolved", "ignored", "all"] as const satisfies readonly StatusFilter[];

export function issuesSearch(s: Record<string, unknown>): IssuesSearch {
  return {
    project: text(s.project),
    status: oneOf(s.status, STATUSES),
    environment: text(s.environment),
    release: text(s.release),
    q: text(s.q),
    sort: oneOf(s.sort, SORTS),
  };
}

export const issueSearch = (s: Record<string, unknown>): IssueSearch => ({ event: text(s.event) });

/** A time range ending now, picked in the rail. */
export const RANGES = [
  ["15m", "15 minutes", 15 * 60_000],
  ["1h", "1 hour", 3_600_000],
  ["6h", "6 hours", 6 * 3_600_000],
  ["24h", "24 hours", 24 * 3_600_000],
  ["7d", "7 days", 7 * 86_400_000],
  ["30d", "30 days", 30 * 86_400_000],
] as const;
export type Range = (typeof RANGES)[number][0];

export const LEVELS = ["trace", "debug", "info", "warn", "error", "fatal"] as const satisfies readonly LogLevel[];

/** `from` and `to` are a zoomed range in nanoseconds; without them the range ends now. */
export type LogsSearch = {
  q?: string | undefined;
  project?: string | undefined;
  level?: LogLevel | undefined;
  range?: Range | undefined;
  from?: string | undefined;
  to?: string | undefined;
  live?: "1" | undefined;
};

const ns = (v: unknown) => (typeof v === "string" && /^\d{1,19}$/.test(v) ? v : undefined);

export function logsSearch(s: Record<string, unknown>): LogsSearch {
  const from = ns(s.from);
  const to = ns(s.to);
  const zoomed = from !== undefined && to !== undefined && BigInt(from) <= BigInt(to);
  return {
    q: text(s.q),
    project: text(s.project),
    level: oneOf(s.level, LEVELS),
    range: oneOf(s.range, RANGES.map(([r]) => r)),
    from: zoomed ? from : undefined,
    to: zoomed ? to : undefined,
    live: !zoomed && s.live === "1" ? "1" : undefined,
  };
}

export type MonitorsSearch = { project?: string | undefined };
export const monitorsSearch = (s: Record<string, unknown>): MonitorsSearch => ({ project: text(s.project) });

export type AlertsSearch = { channel?: string | undefined };
export const alertsSearch = (s: Record<string, unknown>): AlertsSearch => ({
  channel: typeof s.channel === "string" && /^\d{1,18}$/.test(s.channel) ? s.channel : undefined,
});

/** The ledger: one actor's rows, or everyone's. */
export type LedgerSearch = { actor?: string | undefined };
export const ledgerSearch = (s: Record<string, unknown>): LedgerSearch => ({ actor: text(s.actor) });

export type ProjectsSearch = { project?: string | undefined; sdk?: Sdk | undefined };
export const projectsSearch = (s: Record<string, unknown>): ProjectsSearch => ({
  project: text(s.project),
  sdk: oneOf(s.sdk, SDKS),
});

/** The usage page: one project or all, and the days its charts reach back. */
export type UsageSearch = { project?: string | undefined; days?: "90" | undefined };
export const usageSearch = (s: Record<string, unknown>): UsageSearch => ({
  project: text(s.project),
  days: s.days === "90" ? "90" : undefined,
});

export const loginSearch = (s: Record<string, unknown>) => ({ next: text(s.next) });

// The sort a person picked last, kept on this device.
const SORT_KEY = "davy.sort";

export function savedSort(): Sort {
  try {
    return oneOf(localStorage.getItem(SORT_KEY), SORTS) ?? "last_seen";
  } catch {
    return "last_seen";
  }
}

export function saveSort(sort: Sort) {
  try {
    localStorage.setItem(SORT_KEY, sort);
  } catch {
    // A browser that keeps no storage still sorts, it only forgets.
  }
}

/** The list filters of the list the person came from, for the way back. */
export let lastList: IssuesSearch = {};
export function rememberList(search: IssuesSearch) {
  lastList = search;
}

/** URLSearchParams in both directions, so `?q=123` stays the text `123`. */
export function parseSearch(s: string): Record<string, unknown> {
  return Object.fromEntries(new URLSearchParams(s));
}

export function stringifySearch(o: Record<string, unknown>): string {
  const q = new URLSearchParams();
  for (const [key, value] of Object.entries(o)) {
    if (typeof value === "string" && value !== "") q.set(key, value);
  }
  const s = q.toString();
  return s ? `?${s}` : "";
}
