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

test("a person signs in, finds the SDK's issue, reads its frame and resolves it", async ({ page }) => {
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

  await test.step("the list shows the issues the Sentry SDKs sent", async () => {
    await expect(page.locator("tr.row")).toHaveCount(4);
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
    await expect(page.locator("tr.row")).toHaveCount(3);
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
    await expect(rows).toHaveCount(11);
    await expect(rows.filter({ hasText: "sentry payment provider timed out" })).toHaveCount(1);
    await noAxeViolations(page, "logs");

    const search = page.getByRole("searchbox", { name: "Search messages and attributes" });
    await search.fill("attributes.provider:acme");
    await expect(page).toHaveURL(/q=attributes\.provider/);
    await expect(rows).toHaveCount(1);
    await rows.first().locator(".lhead").click();
    await expect(page.locator(".ldet")).toContainText("acme-pay");

    await search.fill("");
    await expect(rows).toHaveCount(11);
    await rows.filter({ hasText: "sentry order placed" }).locator(".lhead").click();
    await page.getByRole("button", { name: "Search for order.id ord-1001" }).click();
    await expect(search).toHaveValue('attributes.order\\.id:"ord-1001"');
    await expect(rows).toHaveCount(1);
    await search.fill("");
    await expect(rows).toHaveCount(11);

    const box = (await page.locator(".hist svg").boundingBox())!;
    await page.mouse.move(box.x + 1, box.y + box.height / 2);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width - 1, box.y + box.height / 2, { steps: 8 });
    await page.mouse.up();
    await expect(page).toHaveURL(/from=\d+&.*to=\d+|to=\d+&.*from=\d+/);
    await expect(page.getByRole("button", { name: "Zoom out" })).toBeVisible();
    await expect(rows).toHaveCount(11);
    await page.keyboard.press("Escape");
    await expect(page.getByRole("button", { name: "Zoom out" })).toHaveCount(0);

    await page.keyboard.press("l");
    await expect(page.getByRole("button", { name: /^Live/ })).toHaveAttribute("aria-pressed", "true");
    const dsn = davy("project", "create", "live-test", "--public-url", URL).trim();
    const key = new globalThis.URL(dsn).username;
    const sent = await page.request.post(`${URL}/`, { headers: { authorization: `Bearer ${key}` }, data: { message: "live line" } });
    expect(sent.status()).toBe(202);
    await expect(rows.filter({ hasText: "live line" })).toHaveCount(1, { timeout: 6000 });
    await expect(rows).toHaveCount(12);
    await noAxeViolations(page, "logs, live");
    await scheme(page, "dark");
    await noAxeViolations(page, "logs, dark");
    await scheme(page, "light");
  });

  expect(problems, "page errors and CSP violations").toEqual([]);
});
