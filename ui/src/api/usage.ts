// What `GET /ui/api/usage` answers: the JSON of `davy usage --json`, sizes in
// bytes. Written by hand, as the report builds its JSON without a Rust type.

export type Count = { records: number; bytes: number; records_per_day: number; bytes_per_day: number };

export const SIGNALS = ["errors", "logs", "sessions", "feedback", "checkins"] as const;
export type SignalName = (typeof SIGNALS)[number];

export type UsageProject = {
  project: string;
  logs_days: number;
  errors_days: number;
  signals: Record<SignalName, Count>;
  logs_stored: Count;
  logs_filtered: Count;
  errors_discarded: number;
  limits: Limits;
};

/** Errors in events, logs in bytes received, on the UTC day open now. */
export type QuotaUse = { quota: number; today: number; percent: number; refusing: boolean };

/** A spike rate: error events or log records a second, null when off. */
export type SpikeRate = { setting: "default" | "off" | "set"; per_second: number | null };

/** Today's refusals by reason. */
export type Refused = { davy_quota: number; davy_spike: number };

/** A project's daily quotas against today's use, its spike rates and today's refusals. */
export type Limits = {
  errors: QuotaUse | null;
  logs: QuotaUse | null;
  warn_percent: number;
  spike: { errors: SpikeRate; logs: SpikeRate };
  resets_at: string;
  refused_today: Refused;
  refused_log_requests_today: Refused;
  busy_refused_requests_today: number;
};

export type Disk = {
  used: number;
  free: number;
  total: number;
  headroom: number;
  headroom_reason: string;
  /** What may still be used before total minus headroom. */
  room_left: number;
  /** The filesystem uses total minus headroom or more already. */
  past_limit: boolean;
  steady_state: number | null;
  days_left: number | null;
  worst_case: boolean | null;
  full_on: string | null;
  beyond_10_years: boolean | null;
  partial: { skipped: string[]; skipped_count: number; stopped: string | null } | null;
};

export type Basis = {
  confidence: "low" | "medium" | "high" | null;
  full_on_range: [string | null, string | null];
  logs: { basis: string };
  errors: { basis: string } | null;
  why: string;
};

export type UsageReport = {
  window: { since: string; days: number };
  growth_per_day: number | null;
  forecast: string;
  projects: UsageProject[];
  /** Busy 429s refused today before a key was read. */
  busy_refused_requests_today_no_project: number;
  disk?: Disk;
  disk_error?: string;
  basis: Basis | null;
  no_forecast?: string;
};

export type PatternEntry = {
  kind: "pattern";
  id: string;
  project: string | null;
  service: string | null;
  level: string | null;
  example: string;
  bytes: number;
  per_day: number;
  share: number;
  last_queried: string | null;
  try: string;
  suggest: string | null;
  /** The rule in words, null when the view suggests none. */
  rule: string | null;
  /** The command a person runs to add the rule. */
  command: string | null;
  saves_per_day: number | null;
};

export type ShadowEntry = {
  kind: "shadow";
  id: number;
  project: string;
  by: string;
  pattern: string | null;
  level: string | null;
  message: string | null;
  service: string | null;
  keep_percent: number;
  hours: number;
  would_drop: { records: number; bytes: number };
  saves_per_day: number | null;
  hides: string | null;
  suggest: string | null;
  rule: string;
  /** The command that makes it live, null when it would hide something. */
  command: string | null;
};

export type RetentionEntry = {
  kind: "retention";
  project: string;
  logs_days: number;
  oldest_read_days: number;
  searches: number;
  history_days: number;
  suggest: string;
  command: string;
  saves: number;
  try: string;
};

export type IssueEntry = {
  kind: "issue";
  id: number;
  project: string;
  title: string;
  events: number;
  share: number;
  suggest: string;
  command: string;
  try: string;
};

export type SavingsEntry = PatternEntry | ShadowEntry | RetentionEntry | IssueEntry;

/** What `GET /ui/api/usage?view=savings` answers. */
export type Savings = {
  window: { since: string; days: number };
  savings: SavingsEntry[];
  note: string;
  patterns_partial?: boolean;
};
