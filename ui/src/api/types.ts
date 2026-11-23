// Made from the Rust types by `DAVY_WRITE_TYPES=1 cargo test ui::types`.
// Do not edit.

export type Me = { email: string, };

export type Login = { email: string, password: string, next?: string, };

export type LoggedIn = { email: string, next: string, };

export type Welcome = { secret: string, password: string, };

export type NewPassword = { current: string, new: string, };

export type Sort = "last_seen" | "events" | "first_seen";

export type Status = "unresolved" | "resolved" | "ignored";

export type Project = { slug: string, 
/**
 * The [`RELEASES`] releases seen first most recently.
 */
releases: Array<string>, };

export type IssueRow = { id: number, project: string, title: string, culprit: string | null, level: string, status: Status, count: number, first_seen: string, last_seen: string, 
/**
 * Events in each of the [`HOURS`] hours from `IssuePage::hours_from`.
 */
hourly: Array<number>, };

export type IssuePage = { issues: Array<IssueRow>, 
/**
 * The cursor of the next page, when there is one.
 */
next: string | null, 
/**
 * The start of the first hour of every row's hourly chart.
 */
hours_from: string, };

export type IssueDetail = { id: number, project_id: number, project: string, title: string, culprit: string | null, level: string, status: Status, resolved_in_release: string | null, resolved_at: string | null, regressed_at: string | null, first_seen: string, last_seen: string, event_count: number, };

export type ReleaseCount = { release: string, events: number, last_seen: string, };

export type IssueView = { issue: IssueDetail, hourly: Array<number>, hours_from: string, releases: Array<ReleaseCount>, };

export type EventItem = { event_id: string, timestamp: string, release: string | null, environment: string | null, 
/**
 * Whether Davy kept the payload. Only a sample of events keep one.
 */
stored: boolean, };

export type EventPage = { events: Array<EventItem>, next: string | null, };

export type Cut<T> = { items: Array<T>, more: number, };

export type EventView = { event_id: string, timestamp: string | null, level: string, platform: string | null, release: string | null, environment: string | null, message: string | null, trace_id: string | null, 
/**
 * In the order the SDK sent them, the exception that was thrown last
 * at the end.
 */
exceptions: Cut<ExceptionView>, breadcrumbs: Cut<Breadcrumb>, tags: Cut<Pair>, request: Request | null, user: Cut<Pair>, 
/**
 * Runtime, os and browser first, then the rest by name.
 */
contexts: Cut<Context>, };

export type ExceptionView = { type: string | null, value: string | null, mechanism: string | null, handled: boolean | null, 
/**
 * The innermost frame, where it was thrown, at the end.
 */
frames: Cut<FrameView>, };

export type FrameView = { function: string | null, module: string | null, filename: string | null, abs_path: string | null, lineno: number | null, colno: number | null, in_app: boolean, pre_context: Array<string>, context_line: string | null, post_context: Array<string>, 
/**
 * Where the frame was in the minified file, when a source map moved
 * it, as `file:line:column`.
 */
minified: string | null, };

export type Breadcrumb = { timestamp: string | null, type: string | null, category: string | null, level: string | null, message: string | null, data: Cut<Pair>, };

export type Pair = { key: string, value: string, };

export type Request = { method: string | null, url: string | null, };

export type Context = { name: string, fields: Cut<Pair>, };

export type LogSource = "logtail" | "otlp" | "sentry";

export type LogLevel = "trace" | "debug" | "info" | "warn" | "error" | "fatal";

export type JsonValue = number | string | boolean | Array<JsonValue> | { [key in string]: JsonValue } | null;

export type LogLine = { timestamp: string, timestamp_ns: string, project: string, source: LogSource, level: LogLevel, service: string | null, message: string, trace_id: string | null, span_id: string | null, attributes: { [key in string]: JsonValue }, };

export type Logs = { 
/**
 * Which logs these are, in words.
 */
scope: string, 
/**
 * Oldest first.
 */
logs: Array<LogLine>, };

export type LogPage = { 
/**
 * Newest first.
 */
logs: Array<LogLine>, 
/**
 * The cursor of the page before this one, when it is full.
 */
before: string | null, };

export type LogHistogram = { from: string, step: string, counts: Array<number>, };

export type StatusChange = { issues: Array<number>, status: Status, release?: string, };
