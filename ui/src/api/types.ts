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
hourly: Array<number>, 
/**
 * When a person discarded the issue, null when it is not discarded.
 */
discarded_at: string | null, };

export type IssuePage = { issues: Array<IssueRow>, 
/**
 * The cursor of the next page, when there is one.
 */
next: string | null, 
/**
 * The start of the first hour of every row's hourly chart.
 */
hours_from: string, };

export type IssueDetail = { id: number, project_id: number, project: string, title: string, culprit: string | null, level: string, status: Status, resolved_in_release: string | null, resolved_at: string | null, regressed_at: string | null, first_seen: string, last_seen: string, event_count: number, 
/**
 * When a person discarded the issue, null when it is not discarded:
 * a discarded issue's new events are counted, not stored, so its
 * `last_seen` stops while the error goes on, see src/discard.rs.
 */
discarded_at: string | null, };

export type ReleaseCount = { release: string, events: number, last_seen: string, };

export type IssueView = { issue: IssueDetail, hourly: Array<number>, hours_from: string, releases: Array<ReleaseCount>, };

export type EventItem = { event_id: string, timestamp: string, release: string | null, environment: string | null, 
/**
 * Whether Davy kept the payload. The newest event and a sample of the rest keep one.
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

export type LogSource = "logtail" | "otlp" | "sentry" | "vercel" | "davy";

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

export type MonitorKind = "http" | "heartbeat";

export type State = "pending" | "up" | "failing" | "down";

export type Hour = { checks: number, failed: number, 
/**
 * The mean response time of the checks that have one, in milliseconds.
 */
ms: number | null, 
/**
 * An incident was open during some of the hour. A failed check alone
 * does not make an hour down, as it does not lower the uptime.
 */
down: boolean, };

export type MonitorRow = { id: number, project: string, name: string, kind: MonitorKind, 
/**
 * The scheme, host and path an HTTP monitor checks. The query and a
 * login may hold a credential, so they stay out, and a heartbeat's ping
 * URL is one, so it has none.
 */
url: string | null, state: State, 
/**
 * How often an HTTP monitor is checked, or the interval of a
 * heartbeat; 0 for a heartbeat on a calendar schedule.
 */
every_s: number, 
/**
 * What a heartbeat expects, such as `every 1h` or
 * `cron 0 * * * *, Europe/Prague`.
 */
schedule: string | null, 
/**
 * The slug of a heartbeat that takes Sentry SDK check-ins.
 */
slug: string | null, 
/**
 * When Davy last checked an HTTP monitor, or a heartbeat last pinged.
 */
last_heard: string | null, 
/**
 * When the open incident started.
 */
open_since: string | null, 
/**
 * Checks in each of the [`HOURS`] hours from `hours_from`.
 */
hourly: Array<Hour>, };

export type MonitorPage = { monitors: Array<MonitorRow>, 
/**
 * The start of the first hour of every row's hourly chart.
 */
hours_from: string, };

export type Incident = { id: number, started: string, resolved: string | null, reason: string, };

export type Check = { at: string, ok: boolean, status: number | null, ms: number | null, reason: string | null, };

export type MonitorDetail = { monitor: MonitorRow, hours_from: string, 
/**
 * The share of the last 7 days without an open incident, from 0 to 1,
 * counted from when the monitor was added. None for a monitor added
 * this millisecond.
 */
uptime_7d: number | null, uptime_30d: number | null, timeout_s: number | null, grace_s: number | null, last_ping: string | null, 
/**
 * When a heartbeat that gets no ping opens an incident: after its
 * interval and grace, at its next scheduled time plus margin, or at the
 * end of the maximum runtime of an open run. None before its first ping.
 */
expected_by: string | null, 
/**
 * The [`NEWEST`] newest incidents.
 */
incidents: Array<Incident>, 
/**
 * The [`NEWEST`] newest checks.
 */
checks: Array<Check>, };

export type Kind = "webhook" | "slack" | "email";

export type Channel = { id: number, 
/**
 * `None` for a channel of every project.
 */
project: string | null, kind: Kind, 
/**
 * The host of a webhook or Slack URL, because the whole URL of a Slack
 * webhook is a credential, or the address of an e-mail channel.
 */
to: string, feedback: boolean, unsent: number, failed: number, 
/**
 * Issue, feedback and operator notices not stored because their class
 * was full. Only someone who can make issues or operator alerts moves it.
 */
dropped: number, 
/**
 * Notices of monitors made by check-ins not stored because 100 waited.
 * Anyone with a public DSN can raise it, so it has its own count.
 */
sdk_dropped: number, };

export type Delivery = "waiting" | "sent" | "failed";

export type Notification = { id: number, channel: number, 
/**
 * What it tells, such as `issue.created` or `monitor.down`.
 */
kind: string, created: string, delivery: Delivery, attempts: number, 
/**
 * When it was sent or given up on.
 */
done: string | null, 
/**
 * When the next try is due, while it waits.
 */
next_try: string | null, 
/**
 * Why the last try failed. It never holds the channel's URL.
 */
last_error: string | null, };

export type NotificationPage = { notifications: Array<Notification>, 
/**
 * The cursor of the next, older page, when there is one.
 */
next: string | null, };

export type ProjectKey = { slug: string, 
/**
 * What a Sentry SDK takes.
 */
dsn: string, 
/**
 * The public key in the DSN, which log shippers send as a bearer token.
 */
key: string, };

export type Endpoints = { public_url: string, 
/**
 * Where log shippers POST JSON or NDJSON.
 */
logs: string, 
/**
 * Where an OpenTelemetry exporter POSTs logs over HTTP.
 */
otlp: string, 
/**
 * Where agents connect, when the server has an admin token.
 */
mcp: string, projects: Array<ProjectKey>, };

export type First = "agent" | "project" | "error" | "symbolicated" | "log" | "monitor" | "delivered";

export type Step = { what: First, required: boolean, 
/**
 * When it was first done, or None while it is not.
 */
at: string | null, };

export type Setup = { steps: Array<Step>, };

export type ServerDefault = "default";

export type Setting = number | ServerDefault;

export type RetentionChange = { project: string, logs_days?: Setting, errors_days?: Setting, };

export type Kept = { days: number, 
/**
 * Whether the days come from the server, not the project.
 */
server_default: boolean, 
/**
 * RFC 3339. What is older is deleted: logs by their timestamp, errors
 * by when Davy received them.
 */
deletes_before: string, };

export type Retention = { project: string, logs: Kept, errors: Kept, };

export type Counts = { 
/**
 * Raw bytes of the logs stored on the UTC day of their index, a line
 * indexed for tomorrow on today, and the bytes of the errors,
 * sessions, feedback and check-ins accepted that day.
 */
stored: Array<number>, 
/**
 * Records accepted, of every signal.
 */
accepted: Array<number>, 
/**
 * Log lines live drop rules dropped.
 */
filtered: Array<number>, 
/**
 * Error events of discarded issues, counted and not stored.
 */
discarded: Array<number>, 
/**
 * Items Davy refused past one of its rate limits or daily caps.
 */
limited: Array<number>, 
/**
 * Items Davy accepted and then refused for another reason, such as a
 * check-in for a monitor that does not exist.
 */
refused: Array<number>, 
/**
 * Check-ins Davy could not read, too large or not valid.
 */
unreadable: Array<number>, 
/**
 * Items the SDKs reported they dropped before sending.
 */
sdk_dropped: Array<number>, };

export type ProjectDays = { project: string, 
/**
 * Raw bytes of the logs stored on the UTC day of their index, a line
 * indexed for tomorrow on today, and the bytes of the errors,
 * sessions, feedback and check-ins accepted that day.
 */
stored: Array<number>, 
/**
 * Records accepted, of every signal.
 */
accepted: Array<number>, 
/**
 * Log lines live drop rules dropped.
 */
filtered: Array<number>, 
/**
 * Error events of discarded issues, counted and not stored.
 */
discarded: Array<number>, 
/**
 * Items Davy refused past one of its rate limits or daily caps.
 */
limited: Array<number>, 
/**
 * Items Davy accepted and then refused for another reason, such as a
 * check-in for a monitor that does not exist.
 */
refused: Array<number>, 
/**
 * Check-ins Davy could not read, too large or not valid.
 */
unreadable: Array<number>, 
/**
 * Items the SDKs reported they dropped before sending.
 */
sdk_dropped: Array<number>, };

export type ProjectStored = { project: string, stored: Array<number>, };

export type OtherDays = { 
/**
 * How many projects it sums.
 */
projects: number, 
/**
 * The [`OTHER_TOP`] of them that stored the most bytes in the window,
 * the most first, a tie by age.
 */
top: Array<ProjectStored>, 
/**
 * Raw bytes of the logs stored on the UTC day of their index, a line
 * indexed for tomorrow on today, and the bytes of the errors,
 * sessions, feedback and check-ins accepted that day.
 */
stored: Array<number>, 
/**
 * Records accepted, of every signal.
 */
accepted: Array<number>, 
/**
 * Log lines live drop rules dropped.
 */
filtered: Array<number>, 
/**
 * Error events of discarded issues, counted and not stored.
 */
discarded: Array<number>, 
/**
 * Items Davy refused past one of its rate limits or daily caps.
 */
limited: Array<number>, 
/**
 * Items Davy accepted and then refused for another reason, such as a
 * check-in for a monitor that does not exist.
 */
refused: Array<number>, 
/**
 * Check-ins Davy could not read, too large or not valid.
 */
unreadable: Array<number>, 
/**
 * Items the SDKs reported they dropped before sending.
 */
sdk_dropped: Array<number>, };

export type UsageDays = { 
/**
 * Each UTC day of the window, oldest first, today last.
 */
days: Array<string>, 
/**
 * The slug of every project, oldest first, so a page can list them
 * all and give each the same place whichever is asked for.
 */
names: Array<string>, 
/**
 * The first [`NAMED`] projects, oldest first, or the one asked for.
 */
projects: Array<ProjectDays>, 
/**
 * The projects past the first [`NAMED`], summed, or `None` when there
 * are no more or one project was asked for.
 */
other: OtherDays | null, };
