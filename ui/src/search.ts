// The state of every view lives in the URL, so a link shows what its sender saw.

import type { Sort, Status } from "./api/types";

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
