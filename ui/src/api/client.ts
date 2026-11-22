import { QueryClient } from "@tanstack/react-query";

/** A refusal from Davy, with the words it gave. */
export class Refused extends Error {
  constructor(
    readonly status: number,
    message: string,
    /** Seconds from `Retry-After`, when Davy said how long to wait. */
    readonly retryAfter: number | null = null,
  ) {
    super(message);
  }
}

const OPEN = ["/login", "/logout", "/welcome"];

/** The login page, coming back to where the person is now. */
export function toLogin() {
  const here = window.location.pathname + window.location.search;
  window.location.replace(`/ui/login?next=${encodeURIComponent(here)}`);
}

async function call<T>(method: "GET" | "POST", path: string, body?: unknown): Promise<T> {
  const init: RequestInit = { method, credentials: "same-origin" };
  if (body !== undefined) {
    // Davy takes a write only as JSON, so a form on another site cannot post one.
    init.headers = { "content-type": "application/json" };
    init.body = JSON.stringify(body);
  }
  const res = await fetch(`/ui/api${path}`, init);
  if (res.status === 401 && !OPEN.includes(path)) {
    toLogin();
  }
  if (!res.ok) {
    const said: unknown = await res.json().catch(() => null);
    const error = said && typeof said === "object" && "error" in said ? String(said.error) : res.statusText;
    const wait = Number.parseInt(res.headers.get("retry-after") ?? "", 10);
    throw new Refused(res.status, error || `Davy answered ${res.status}`, Number.isFinite(wait) ? wait : null);
  }
  return res.status === 204 ? (undefined as T) : ((await res.json()) as T);
}

export const get = <T>(path: string) => call<T>("GET", path);
export const post = <T>(path: string, body: unknown) => call<T>("POST", path, body);

/** `?a=1&b=2` without the keys whose value is missing or empty. */
export function query(params: Record<string, string | undefined>): string {
  const q = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value) q.set(key, value);
  }
  const s = q.toString();
  return s ? `?${s}` : "";
}

// Davy answers 429 with Retry-After 1 while ingest holds the memory or the log
// searches are all busy. That passes, so the UI asks again.
const busy = (count: number, error: unknown) => error instanceof Refused && error.status === 429 && count < 10;

export const queries = new QueryClient({
  defaultOptions: {
    queries: { retry: busy, retryDelay: 1000, staleTime: 10_000, refetchOnWindowFocus: true },
    mutations: { retry: busy, retryDelay: 1000 },
  },
});
