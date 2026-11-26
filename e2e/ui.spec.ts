import { execFileSync } from "node:child_process";
import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Page } from "@playwright/test";

const BIN = need("DAVY_BIN");
const URL = need("DAVY_URL");
const DATA = need("DAVY_DATA");
const EMAIL = "ui@example.com";
const PASSWORD = "correct horse battery staple";

function need(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`e2e/ui.spec.ts needs ${name}; e2e/run.sh sets it`);
  return value;
}

const davy = (...args: string[]) => execFileSync(BIN, [...args, "--data", DATA], { encoding: "utf8" });

const problems: string[] = [];

function watch(page: Page) {
  page.on("pageerror", (e) => problems.push(`page error: ${e.message}`));
  page.on("console", (m) => {
    if (m.text().includes("Content Security Policy")) problems.push(`CSP: ${m.text()}`);
  });
}

async function noAxeViolations(page: Page, view: string) {
  const { violations } = await new AxeBuilder({ page }).withTags(["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"]).analyze();
  expect(
    violations.flatMap((v) => v.nodes.map((n) => `${v.id} at ${n.target.join(" ")}: ${n.failureSummary ?? ""}`)),
    `axe on ${view}`,
  ).toEqual([]);
}

// Buttons fade their colours, so axe would measure a colour half way between
// the two schemes.
async function scheme(page: Page, colorScheme: "light" | "dark") {
  await page.emulateMedia({ colorScheme });
  await page.waitForFunction(() => document.getAnimations().length === 0);
}

test("a person signs in, finds the SDK's issue, reads its frame and resolves it", async ({ page, browser }) => {
  // One journey through every page, with axe in both schemes on each.
  test.setTimeout(120_000);
  watch(page);
  await page.addInitScript(() =>
    document.addEventListener("securitypolicyviolation", (e) => console.error(`Content Security Policy: ${e.violatedDirective} ${e.blockedURI}`)),
  );

  const link = davy("user", "add", EMAIL, "--public-url", URL).match(/http\S*\/ui\/welcome#\S+/)?.[0];
  expect(link, "davy user add prints a welcome link").toBeTruthy();

  await test.step("the welcome link sets a password and signs in", async () => {
    await page.goto(link!);
    await expect(page).toHaveURL(`${URL}/ui/welcome`);
    await noAxeViolations(page, "welcome");
    await page.fill("#password", PASSWORD);
    await page.fill("#again", PASSWORD);
    await page.click("button[type=submit]");
    await expect(page).toHaveURL(`${URL}/ui/issues`);
  });

  const row = page.locator("tr.row", { hasText: "sentry-node-7" }).filter({ hasText: "TypeError: SDK test failure" });

  // Every sender in run.sh adds issues, so the list must match the store.
  const unresolved = () => JSON.parse(davy("sql", "--json", "SELECT count(*) FROM issues WHERE status = 'unresolved'")).rows[0][0] as number;
  const listed = unresolved();

  await test.step("the list shows the issues the Sentry SDKs sent", async () => {
    expect(listed).toBeGreaterThan(1);
    await expect(page.locator("tr.row")).toHaveCount(listed);
    await expect(row.locator("td").nth(5)).toHaveText("2");
    await noAxeViolations(page, "issues list");
    await scheme(page, "dark");
    await noAxeViolations(page, "issues list, dark");
    await scheme(page, "light");
  });

  await test.step("the issue page shows the in-app frame with its crash line", async () => {
    await row.getByRole("link").click();
    await expect(page.getByRole("heading", { level: 1 })).toHaveText("TypeError: SDK test failure");
    const frame = page.locator(".frame.mine.open").first();
    await expect(frame.locator(".fnm")).toHaveText("capture");
    await expect(frame.locator(".loc")).toContainText("e2e/send.mjs:5:");
    await expect(frame.locator(".ln.hit")).toContainText("throw new Type('SDK test failure')");
    await noAxeViolations(page, "issue");
    await scheme(page, "dark");
    await noAxeViolations(page, "issue, dark");
    await scheme(page, "light");
  });

  await test.step("r resolves it, the store says so and the list drops it", async () => {
    const id = page.url().split("/").at(-1)!;
    await page.keyboard.press("r");
    await expect(page.locator(".ihead .tags")).toContainText("resolved");
    expect(JSON.parse(davy("sql", "--json", `SELECT status FROM issues WHERE id = ${Number(id)}`)).rows).toEqual([["resolved"]]);
    await page.keyboard.press("Escape");
    await expect(page).toHaveURL(`${URL}/ui/issues`);
    await expect(page.locator("tr.row")).toHaveCount(listed - 1);
    await expect(row).toHaveCount(0);
  });

  await test.step("after logging out a wrong password is refused and the right one signs in", async () => {
    await page.getByRole("button", { name: "Log out" }).click();
    await expect(page).toHaveURL(/\/ui\/login/);
    await noAxeViolations(page, "login");
    await page.fill("#email", EMAIL);
    await page.fill("#password", "not the password");
    await page.click("button[type=submit]");
    await expect(page.locator(".problem")).toHaveText("wrong e-mail or password");
    await page.waitForTimeout(1100);
    await page.fill("#password", PASSWORD);
    await page.click("button[type=submit]");
    await expect(page).toHaveURL(`${URL}/ui/issues`);
  });

  await test.step("the Logs page finds a log by its attribute, zooms and goes live", async () => {
    await page.getByRole("navigation", { name: "Views" }).getByRole("link", { name: "Logs" }).click();
    await expect(page).toHaveURL(`${URL}/ui/logs`);
    const rows = page.locator(".lrow");
    // The 11 logs the senders sent, and the line Davy recorded in its own
    // project when it made it at start.
    const shown = 12;
    await expect(rows).toHaveCount(shown);
    await expect(rows.filter({ hasText: 'created project "davy"' })).toHaveCount(1);
    await expect(rows.filter({ hasText: "sentry payment provider timed out" })).toHaveCount(1);
    await noAxeViolations(page, "logs");

    const search = page.getByRole("searchbox", { name: "Search messages and attributes" });
    await search.fill("attributes.provider:acme");
    await expect(page).toHaveURL(/q=attributes\.provider/);
    await expect(rows).toHaveCount(1);
    await rows.first().locator(".lhead").click();
    await expect(page.locator(".ldet")).toContainText("acme-pay");

    await search.fill("");
    await expect(rows).toHaveCount(shown);
    await rows.filter({ hasText: "sentry order placed" }).locator(".lhead").click();
    await page.getByRole("button", { name: "Search for order.id ord-1001" }).click();
    await expect(search).toHaveValue('attributes.order\\.id:"ord-1001"');
    await expect(rows).toHaveCount(1);
    await search.fill("");
    await expect(rows).toHaveCount(shown);

    const box = (await page.locator(".hist svg").boundingBox())!;
    await page.mouse.move(box.x + 1, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width - 1, box.y + box.height / 2, { steps: 8 });
    await page.mouse.up();
    await expect(page).toHaveURL(/from=\d+&.*to=\d+|to=\d+&.*from=\d+/);
    await expect(page.getByRole("button", { name: "Zoom out" })).toBeVisible();
    await expect(rows).toHaveCount(shown);
    await page.keyboard.press("Escape");
    await expect(page.getByRole("button", { name: "Zoom out" })).toHaveCount(0);

    await page.keyboard.press("l");
    await expect(page.getByRole("button", { name: /^Live/ })).toHaveAttribute("aria-pressed", "true");
    const dsn = davy("project", "create", "live-test", "--public-url", URL).trim();
    const key = new globalThis.URL(dsn).username;
    const sent = await page.request.post(`${URL}/`, { headers: { authorization: `Bearer ${key}` }, data: { message: "live line" } });
    expect(sent.status()).toBe(202);
    await expect(rows.filter({ hasText: "live line" })).toHaveCount(1, { timeout: 6000 });
    await expect(rows).toHaveCount(shown + 1);
    await noAxeViolations(page, "logs, live");
    await scheme(page, "dark");
    await noAxeViolations(page, "logs, dark");
    await scheme(page, "light");
  });

  const views = page.getByRole("navigation", { name: "Views" });
  const both = async (view: string) => {
    await noAxeViolations(page, view);
    await scheme(page, "dark");
    await noAxeViolations(page, `${view}, dark`);
    await scheme(page, "light");
  };

  await test.step("Uptime lists the monitors and a heartbeat's page shows its ping", async () => {
    davy("monitor", "add", "logs", "login page", "--url", `${URL}/ui/login`);
    const pingPath = davy("monitor", "add", "logs", "nightly", "--heartbeat").trim().split("\t")[1]!;
    expect((await page.request.post(`${URL}${pingPath}`)).status()).toBe(200);
    await views.getByRole("link", { name: "Uptime" }).click();
    await expect(page).toHaveURL(`${URL}/ui/monitors`);
    const logsRows = page.locator("tr.row").filter({ has: page.locator(".pj", { hasText: /^logs$/ }) });
    await expect(logsRows).toHaveCount(2);
    const checkIns = page.locator("tr.row", { hasText: "check-ins for nightly-report" });
    await expect(checkIns).toHaveCount(2);
    await expect(checkIns.first()).toContainText("cron 0 * * * *, Europe/Prague");
    const nightly = logsRows.filter({ hasText: "nightly" });
    await expect(nightly).toContainText("heartbeat, a ping every 1h");
    await expect(nightly.locator(".stt")).toHaveText("up");
    await both("uptime");
    await nightly.getByRole("link", { name: "nightly" }).click();
    await expect(page.getByRole("heading", { level: 1 })).toHaveText("nightly");
    await expect(page.locator(".ihead .sub")).toHaveText("Heartbeat. A ping every 1h, late after 5m more");
    await expect(page.locator(".kv")).toContainText("Last ping");
    await both("monitor");
    await page.keyboard.press("Escape");
    await expect(page).toHaveURL(`${URL}/ui/monitors`);
  });

  await test.step("Alerts shows a channel and the test notice queued for it", async () => {
    const id = davy("channel", "add", "webhook", "http://127.0.0.1:9/hook", "--all").trim().split("\t")[0]!;
    davy("channel", "test", id);
    await views.getByRole("link", { name: "Alerts" }).click();
    await expect(page.locator("#channels + div tbody tr")).toHaveCount(1);
    await expect(page.locator("#channels + div tbody tr")).toContainText("127.0.0.1");
    await expect(page.locator("#sent + div tbody tr").filter({ hasText: "test" })).toHaveCount(1);
    await both("alerts");
  });

  await test.step("Projects shows the DSN and the Python setup for a project", async () => {
    await views.getByRole("link", { name: "Projects" }).click();
    await page.getByRole("button", { name: "logs", exact: true }).click();
    const dsn = JSON.parse(davy("sql", "--json", "SELECT public_key FROM projects WHERE slug = 'logs'")).rows[0][0] as string;
    await expect(page.locator(".kv.copyable").nth(1)).toContainText(dsn);
    await page.getByRole("tab", { name: "Node" }).focus();
    await page.keyboard.press("ArrowRight");
    await page.keyboard.press("ArrowRight");
    await expect(page.getByRole("tab", { name: "Python" })).toBeFocused();
    await expect(page.getByRole("tabpanel")).toContainText("sentry_sdk.init(");
    await expect(page).toHaveURL(/sdk=python/);
    await both("projects");
  });

  await test.step("Usage shows the disk, a project, a savings pattern, the outcomes and the chart as a table", async () => {
    // Pattern counters reach the database on their own schedule, so wait for
    // the savings view to list one before the page is read.
    const patterns = () => {
      const view = JSON.parse(davy("usage", "--project", "logs", "--view", "savings", "--json")) as { savings: { kind: string }[] };
      return view.savings.filter((e) => e.kind === "pattern").length;
    };
    await expect.poll(patterns, { timeout: 15_000 }).toBeGreaterThan(0);
    // Counts per day reach the database every few seconds, so wait for the
    // days answer to hold what the logs project sent.
    const accepted = async () => {
      const r = await page.request.get(`${URL}/ui/api/usage/days?project=logs`);
      const d = (await r.json()) as { projects: { accepted: number[] }[] };
      return d.projects[0]?.accepted.reduce((a, b) => a + b, 0) ?? 0;
    };
    await expect.poll(accepted, { timeout: 15_000 }).toBeGreaterThan(0);
    await views.getByRole("link", { name: "Usage" }).click();
    await expect(page).toHaveURL(`${URL}/ui/usage`);
    // With every project no pattern can say a drop command, so the page asks for one.
    const savings = page.getByRole("region", { name: /^Savings/ });
    const patternGroup = savings.getByRole("region", { name: "Log patterns" });
    await expect(patternGroup.locator(".svlead")).toHaveText("Pick a project to get a drop command.");
    await page.getByRole("button", { name: "logs", exact: true }).click();
    await expect(page).toHaveURL(`${URL}/ui/usage?project=logs`);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText("Usage of logs");

    // The disk's sizes and its day depend on the runner, so only the shape.
    const disk = page.getByRole("region", { name: /^Disk/ });
    await expect(disk.locator("dt").filter({ hasText: /^Full$/ })).toHaveCount(1);
    await expect(disk.getByTestId("fills")).not.toBeEmpty();
    await expect(disk.locator("dt").filter({ hasText: /^Room left$/ })).toHaveCount(1);
    await expect(disk.getByTestId("room")).toContainText("Davy keeps");

    const rows = page.locator("#uprojects tbody tr");
    await expect(rows).toHaveCount(1);
    await expect(rows.locator("th")).toHaveText("logs");
    await expect(rows).toContainText("logs 30 days");

    await expect(savings.locator(".lede")).toHaveText("Davy changes nothing from this page. Copy a command and run it on the server.");
    await expect(patternGroup.locator(".svlead")).toHaveCount(0);
    const pattern = patternGroup.locator(".sv").first();
    await expect(pattern.locator(".try code")).toHaveText(/^search_logs pattern=[0-9a-f]{12}$/);
    await expect(pattern.getByRole("button", { name: "Copy agent query" })).toBeVisible();

    // Today is still counting, whenever the counts were last written.
    const chart = page.locator(".stack svg");
    await expect(chart.locator("text.today")).toHaveText("today, so far");
    await expect(page.locator(".sread")).toHaveAttribute("aria-live", "polite");
    await chart.focus();
    await page.keyboard.press("End");
    await expect(page.locator(".sread")).toContainText(/^[A-Z][a-z]{2} \d{1,2} UTC, today so far: /);
    await page.keyboard.press("ArrowLeft");
    await expect(page.locator(".sread")).toContainText(/^[A-Z][a-z]{2} \d{1,2} UTC: .* stored/);

    const outcomes = page.getByRole("region", { name: /^What became of what was sent/ });
    await expect(outcomes.locator(".lede")).toHaveText(
      "Accepted counts every item Davy took in. Filtered, discarded, rate limited and refused are parts of it that Davy then did not store. Unreadable items and those the SDKs dropped never reached it.",
    );
    const head = page.locator("#uoutcomes thead tr");
    await expect(head.nth(0).locator("th")).toHaveText(["Day, UTC", "Accepted", "Of those, not stored", "Never accepted"]);
    await expect(head.nth(1).locator("th")).toHaveText(["Filtered", "Discarded", "Rate limited", "Refused", "Unreadable", "Dropped by SDK"]);
    const every = outcomes.getByRole("button", { name: "Show all 31 days" });
    await every.click();
    await expect(outcomes.getByRole("button", { name: "Show the newest 14 days" })).toHaveAttribute("aria-pressed", "true");
    await both("usage");

    await page.getByRole("button", { name: "Show table" }).click();
    const table = page.getByRole("region", { name: "Stored bytes per day of logs, UTC" });
    // 30 days back and today.
    await expect(table.locator("tbody tr")).toHaveCount(31);
    await expect(table.locator("thead th")).toHaveText(["Day, UTC", "logs"]);
    await expect(table.locator("tbody tr").first().locator("th")).toHaveText(/^[A-Z][a-z]{2} \d{1,2}, so far$/);
    await expect(page.getByRole("button", { name: "Show chart" })).toHaveAttribute("aria-pressed", "true");
    // Scrolled to its end, the column headers stay on top and a row's
    // header leaves with its row.
    await table.evaluate((el) => el.scrollTo(0, el.scrollHeight));
    const region = (await table.boundingBox())!;
    const first = (await table.locator("tbody tr").first().locator("th").boundingBox())!;
    const top = (await table.locator("thead th").first().boundingBox())!;
    expect(first.y + first.height).toBeLessThan(region.y);
    expect(Math.abs(top.y - region.y)).toBeLessThan(2);
    await noAxeViolations(page, "usage, table");
  });

  await test.step("Setup shows the done steps and the open ones", async () => {
    await views.getByRole("link", { name: "Setup" }).click();
    const step = (title: string) => page.locator(".steps li", { has: page.getByRole("heading", { name: title }) });
    await expect(step("Create a project")).toContainText("Done");
    await expect(step("Send the first error")).toContainText("Done");
    await expect(step("Connect an agent")).toContainText("Not done yet.");
    await expect(step("Connect an agent")).toContainText(`davy mcp-config --client claude-code --url ${URL}`);
    await both("setup");
  });

  await test.step("a password change logs out every other session", async () => {
    const other = await browser.newContext();
    const second = await other.newPage();
    await second.goto(`${URL}/ui/login`);
    await second.fill("#email", EMAIL);
    await second.fill("#password", PASSWORD);
    await second.click("button[type=submit]");
    await expect(second).toHaveURL(`${URL}/ui/issues`);

    await page.getByRole("link", { name: /^Account/ }).click();
    await expect(page.getByRole("heading", { level: 1 })).toHaveText("Account");
    await both("account");
    await page.fill("#current", PASSWORD);
    await page.fill("#new", "short");
    await page.fill("#again", "short");
    await page.getByRole("button", { name: "Change password" }).click();
    await expect(page.locator(".problem")).toHaveText("The new password needs at least 12 characters.");
    await page.fill("#new", `${PASSWORD} two`);
    await page.fill("#again", `${PASSWORD} two`);
    await page.getByRole("button", { name: "Change password" }).click();
    await expect(page.getByRole("status").filter({ hasText: "Password changed" })).toHaveText("Password changed. Every other session is logged out.");
    expect((await second.request.get(`${URL}/ui/api/me`)).status()).toBe(401);
    expect((await page.request.get(`${URL}/ui/api/me`)).status()).toBe(200);
    await other.close();
  });

  expect(problems, "page errors and CSP violations").toEqual([]);
});
