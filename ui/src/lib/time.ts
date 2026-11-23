// Every time in the UI is UTC, like the API and the logs.

export const HOUR = 3_600_000;
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

const pad = (n: number, width = 2) => String(n).padStart(width, "0");

export function day(ms: number): string {
  const d = new Date(ms);
  return `${MONTHS[d.getUTCMonth()]} ${d.getUTCDate()}`;
}

/** `Nov 20 08:42:01 UTC` */
export function stamp(iso: string): string {
  const d = new Date(iso);
  return `${day(d.getTime())} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())} UTC`;
}

/** `08:42:01.048`, for rows that sit within one day. */
export function clock(iso: string): string {
  const d = new Date(iso);
  return `${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())}.${pad(d.getUTCMilliseconds(), 3)}`;
}

/** How long ago, short: `now`, `5m`, `3h`, `4d`. */
export function ago(iso: string, now = Date.now()): string {
  const minutes = Math.max(0, Math.round((now - Date.parse(iso)) / 60_000));
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  if (hours < 48) return `${hours}h`;
  return `${Math.round(hours / 24)}d`;
}

export function agoLong(iso: string, now = Date.now()): string {
  const short = ago(iso, now);
  return short === "now" ? "just now" : `${short} ago`;
}

/** The hour of day, `08`, of the hour that starts at `ms`. */
export function hourOfDay(ms: number): string {
  return pad(new Date(ms).getUTCHours());
}

/** The words for one hour of a chart, the hour still running ends now. */
export function hourRange(start: number, now = Date.now()): string {
  const from = `${day(start)}, ${hourOfDay(start)}:00 to`;
  if (now < start + HOUR) {
    const d = new Date(now);
    return `${from} ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())} UTC, so far`;
  }
  return `${from} ${hourOfDay(start)}:59 UTC`;
}

// Log times are nanoseconds since 1970, past what a Number holds exactly, so
// they stay decimal strings and BigInt does their arithmetic.

export const NS_PER_MS = 1_000_000n;
export const nsOf = (ms: number) => BigInt(ms) * NS_PER_MS;
export const msOf = (ns: bigint | string) => Number(BigInt(ns) / NS_PER_MS);

/** `08:42:01.048`, or `08:42:01` when `ms` is false, of a time in milliseconds. */
export function hms(ms: number, millis = true): string {
  const d = new Date(ms);
  const s = `${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}:${pad(d.getUTCSeconds())}`;
  return millis ? `${s}.${pad(d.getUTCMilliseconds(), 3)}` : s;
}

/** Whether two times in milliseconds fall on one UTC day. */
export const sameDay = (a: number, b: number) => Math.floor(a / 86_400_000) === Math.floor(b / 86_400_000);

/** The words for a span of time from `from` to `to`, both in milliseconds and included. */
export function span(from: number, to: number, millis: boolean): string {
  if (sameDay(from, to)) return `${day(from)}, ${hms(from, millis)} to ${hms(to, millis)} UTC`;
  return `${day(from)} ${hms(from, millis)} to ${day(to)} ${hms(to, millis)} UTC`;
}
