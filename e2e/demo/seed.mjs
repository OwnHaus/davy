// Fills a running Davy with a day of a made-up recipe app, Pantry: an API in
// Node, a web app in the browser and an import worker in Python. The README
// screenshots and the website are taken from it.
// Usage: node e2e/demo/seed.mjs URL API_DSN WEB_DSN WORKER_DSN
import { randomBytes } from "node:crypto";

const [url, ...dsns] = process.argv.slice(2);
if (dsns.length !== 3) throw new Error("usage: node e2e/demo/seed.mjs URL API_DSN WEB_DSN WORKER_DSN");
const [api, web, worker] = dsns.map((dsn) => {
  const { username, pathname } = new URL(dsn);
  return { key: username, id: pathname.slice(1) };
});

const HOUR = 3_600_000;
const now = Date.now();
// A fixed seed, so every run draws the same day.
let state = 20261123;
const random = () => {
  state = (state * 1_103_515_245 + 12_345) % 2 ** 31;
  return state / 2 ** 31;
};
const pick = (list) => list[Math.floor(random() * list.length)];
const hex = (bytes) => randomBytes(bytes).toString("hex");

const frame = (filename, fn, lineno, source, inApp = true) => {
  const lines = source.trim().split("\n");
  const at = lines.findIndex((l) => l.startsWith(">"));
  const strip = (l) => l.replace(/^>/, "");
  return {
    filename,
    abs_path: filename,
    function: fn,
    lineno,
    colno: 12,
    in_app: inApp,
    pre_context: lines.slice(0, at).map(strip),
    context_line: strip(lines[at]),
    post_context: lines.slice(at + 1).map(strip),
  };
};
const lib = (filename, fn, lineno) => ({ filename, abs_path: filename, function: fn, lineno, colno: 5, in_app: false });

const node = { os: { name: "Linux", version: "6.8.0" }, runtime: { name: "node", version: "v22.12.0" } };
const browser = (name, version, os) => ({ browser: { name, version }, os: { name: os } });
const python = { os: { name: "Linux", version: "6.8.0" }, runtime: { name: "CPython", version: "3.13.1" } };

// Each issue says which project it belongs to, how its events spread over
// the last 24 hours, and the event Davy groups them by.
const issues = [
  {
    project: api,
    rate: (h) => (h < 3 ? 14 : 2),
    release: "pantry-api@2.4.1",
    platform: "node",
    contexts: node,
    request: { method: "POST", url: "https://api.pantry.example/v1/recipes/scale" },
    exception: {
      type: "TypeError",
      value: "Cannot read properties of undefined (reading 'servings')",
      frames: [
        lib("node_modules/express/lib/router/layer.js", "Layer.handle", 95),
        frame("src/routes/recipes.ts", "scaleHandler", 48, `
  const recipe = await recipes.find(req.params.id)
  const target = Number(req.body.servings)
> res.json(scaleRecipe(recipe, target))
})
`),
        frame("src/lib/scale.ts", "scaleRecipe", 12, `
export function scaleRecipe(recipe: Recipe, servings: number) {
  // Imported recipes may have no yield yet.
> const factor = servings / recipe.yield.servings
  return recipe.ingredients.map((i) => ({ ...i, amount: i.amount * factor }))
}
`),
      ],
    },
    logs: ["scaling recipe without a yield", "recipe imported from a URL"],
  },
  {
    project: api,
    rate: (h) => (h >= 6 && h < 9 ? 22 : random() < 0.15 ? 1 : 0),
    release: "pantry-api@2.4.1",
    platform: "node",
    contexts: node,
    request: { method: "GET", url: "https://api.pantry.example/v1/plans/week" },
    exception: {
      type: "Error",
      value: "connect ETIMEDOUT 10.0.3.12:5432",
      frames: [
        lib("node:net", "TCPConnectWrap.afterConnect", 1607),
        lib("node_modules/pg-pool/index.js", "Pool._acquireClient", 45),
        frame("src/db/pool.ts", "query", 31, `
export async function query<T>(sql: string, params: unknown[] = []) {
  const started = performance.now()
> const result = await pool.query<T>(sql, params)
  metrics.observe("db.query", performance.now() - started)
  return result.rows
`),
        frame("src/routes/plans.ts", "weekPlan", 22, `
router.get("/plans/week", async (req, res) => {
> const days = await query<PlanDay>(WEEK_PLAN_SQL, [req.user.id])
  res.json(days)
})
`),
      ],
    },
    logs: ["database pool exhausted, waiting for a client", "query took longer than 5000 ms"],
  },
  {
    project: web,
    rate: (h) => (h < 20 ? 3 : 0),
    release: "pantry-web@1.18.0",
    platform: "javascript",
    contexts: browser("Safari", "18.2", "iOS"),
    request: { method: "GET", url: "https://pantry.example/plan" },
    exception: {
      type: "RangeError",
      value: "Invalid time value",
      frames: [
        lib("node_modules/date-fns/format.js", "format", 351),
        frame("src/components/PlanDay.tsx", "PlanDay", 27, `
export function PlanDay({ day }: { day: PlanDay }) {
  // Safari parses "2026-11-23 18:00" as an invalid date.
> const label = format(new Date(day.cookAt), "EEEE HH:mm")
  return <h3 className="plan-day">{label}</h3>
}
`),
      ],
    },
    logs: [],
  },
  {
    project: web,
    rate: (h) => (h < 2 ? 9 : 0),
    release: "pantry-web@1.18.1",
    platform: "javascript",
    contexts: browser("Chrome", "131.0.0", "Windows"),
    request: { method: "GET", url: "https://pantry.example/recipes/new" },
    exception: {
      type: "SyntaxError",
      value: "Unexpected token '<', \"<!DOCTYPE \"... is not valid JSON",
      frames: [
        frame("src/api/client.ts", "request", 41, `
  const response = await fetch(\`\${API}\${path}\`, init)
  if (response.status === 401) return signIn()
> return (await response.json()) as T
}
`),
        frame("src/pages/NewRecipe.tsx", "importFromUrl", 63, `
async function importFromUrl(link: string) {
  setBusy(true)
> const draft = await request<Draft>("/v1/import", { method: "POST", body: link })
  setDraft(draft)
`),
      ],
    },
    logs: [],
  },
  {
    project: worker,
    rate: (h) => (h % 6 === 1 ? 6 : 0),
    release: "pantry-worker@0.9.3",
    platform: "python",
    contexts: python,
    exception: {
      type: "KeyError",
      value: "'unit'",
      frames: [
        frame("worker/jobs.py", "run", 88, `
    for job in queue.take(batch=20):
        try:
>           HANDLERS[job.kind](job.payload)
        except Retry:
            queue.retry(job)
`),
        frame("worker/import_recipes.py", "parse_ingredient", 54, `
def parse_ingredient(row: dict) -> Ingredient:
    amount = Decimal(row["amount"])
>   unit = UNITS[row["unit"]]
    return Ingredient(name=row["name"].strip(), amount=amount, unit=unit)
`),
      ],
    },
    logs: [],
  },
  {
    project: api,
    rate: (h) => (h > 12 ? 2 : 0),
    release: "pantry-api@2.4.0",
    platform: "node",
    contexts: node,
    level: "warning",
    request: { method: "POST", url: "https://api.pantry.example/v1/pantry/items" },
    exception: {
      type: "ValidationError",
      value: "quantity must be a positive number",
      frames: [
        frame("src/routes/pantry.ts", "addItem", 19, `
router.post("/pantry/items", async (req, res) => {
> const item = PantryItem.parse(req.body)
  res.status(201).json(await pantry.add(req.user.id, item))
})
`),
      ],
    },
    logs: [],
  },
  {
    project: web,
    rate: (h) => (h % 4 === 0 ? 1 : 0),
    release: "pantry-web@1.18.0",
    platform: "javascript",
    contexts: browser("Safari", "17.6", "macOS"),
    request: { method: "GET", url: "https://pantry.example/shopping-list" },
    exception: {
      type: "QuotaExceededError",
      value: "Failed to execute 'setItem' on 'Storage': Setting the value of 'pantry:list' exceeded the quota.",
      frames: [
        frame("src/lib/offline.ts", "saveList", 17, `
export function saveList(list: ShoppingList) {
> localStorage.setItem("pantry:list", JSON.stringify(list))
}
`),
      ],
    },
    logs: [],
  },
];

const breadcrumbs = (at, issue) => {
  const steps = issue.request
    ? [
        { category: "navigation", message: `to ${new URL(issue.request.url).pathname}` },
        { category: "http", message: `${issue.request.method} ${issue.request.url} [500]`, level: "error" },
      ]
    : [{ category: "queue", message: "took 20 jobs from imports" }];
  return steps.map((b, i) => ({ type: "default", level: "info", ...b, timestamp: (at - (steps.length - i) * 1500) / 1000 }));
};

async function post(path, project, body, type) {
  const res = await fetch(`${url}${path}`, {
    method: "POST",
    headers: { "Content-Type": type, "X-Sentry-Auth": `Sentry sentry_key=${project.key}, sentry_version=7`, Authorization: `Bearer ${project.key}` },
    body,
  });
  if (!res.ok) throw new Error(`${path} answered ${res.status}: ${await res.text()}`);
}

function event(issue, at, traceId) {
  const user = `u_${1000 + Math.floor(random() * 400)}`;
  return {
    event_id: hex(16),
    timestamp: at / 1000,
    platform: issue.platform,
    level: issue.level ?? "error",
    release: issue.release,
    environment: "production",
    server_name: issue.project === api ? pick(["api-1", "api-2"]) : undefined,
    user: { id: user },
    request: issue.request,
    contexts: { ...issue.contexts, trace: { trace_id: traceId, span_id: hex(8) } },
    breadcrumbs: { values: breadcrumbs(at, issue) },
    exception: {
      values: [{ type: issue.exception.type, value: issue.exception.value, mechanism: { type: "generic", handled: false }, stacktrace: { frames: issue.exception.frames } }],
    },
  };
}

const logs = [];
const log = (at, level, message, attributes = {}) =>
  logs.push({ dt: new Date(at).toISOString(), level, message, service: "api", ...attributes });

let sent = 0;
for (const issue of issues) {
  for (let h = 23; h >= 0; h--) {
    for (let n = issue.rate(h); n > 0; n--) {
      const at = now - h * HOUR - Math.floor(random() * HOUR);
      const traceId = hex(16);
      const e = event(issue, at, traceId);
      const envelope = `${JSON.stringify({ event_id: e.event_id, sent_at: new Date().toISOString() })}\n${JSON.stringify({ type: "event" })}\n${JSON.stringify(e)}\n`;
      await post(`/api/${issue.project.id}/envelope/`, issue.project, envelope, "application/x-sentry-envelope");
      sent++;
      if (issue.project === api) {
        for (const [i, message] of issue.logs.entries()) log(at - 400 + i * 100, "warn", message, { trace_id: traceId, user_id: e.user.id });
        log(at, "error", `${issue.exception.type}: ${issue.exception.value}`, { trace_id: traceId, user_id: e.user.id, path: new URL(issue.request.url).pathname, status: 500 });
      }
    }
  }
}

// Ordinary traffic around the errors, busier in the day.
const routes = [
  ["GET", "/v1/recipes", 34],
  ["GET", "/v1/recipes/:id", 18],
  ["GET", "/v1/plans/week", 61],
  ["POST", "/v1/plans/week", 88],
  ["GET", "/v1/pantry/items", 22],
  ["POST", "/v1/shopping-list/sync", 140],
];
for (let m = 24 * 60; m > 0; m -= 1) {
  const hour = new Date(now - m * 60_000).getUTCHours();
  const busy = hour >= 6 && hour <= 21 ? 6 : 2;
  for (let n = Math.floor(random() * busy); n > 0; n--) {
    const at = now - m * 60_000 + Math.floor(random() * 60_000);
    const [method, path, base] = pick(routes);
    const duration = Math.round(base * (0.5 + random() * 1.5));
    const status = random() < 0.02 ? 404 : 200;
    log(at, duration > 180 ? "warn" : "info", `${method} ${path} ${status} ${duration}ms`, { method, path, status, duration_ms: duration, user_id: `u_${1000 + Math.floor(random() * 400)}`, region: pick(["fra1", "fra1", "ams1"]) });
  }
  if (m % 60 === 0) log(now - m * 60_000, "info", "nightly digest queued", { job: "digest", recipients: 120 + Math.floor(random() * 30) });
}
logs.sort((a, b) => a.dt.localeCompare(b.dt));
for (let i = 0; i < logs.length; i += 500) {
  await post("/", api, JSON.stringify(logs.slice(i, i + 500)), "application/json");
}
console.log(`seed: sent ${sent} events in ${issues.length} issues and ${logs.length} logs`);
