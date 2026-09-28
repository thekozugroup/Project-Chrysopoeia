#!/usr/bin/env node
/**
 * End-to-end smoke test: drives the first-run flow in a real browser against
 * a running Chrysopoeia server, the way a new user would, and waits until a
 * file has been converted, verified and shown on the overview without a
 * reload.
 *
 * Usage:
 *   node e2e/smoke.mjs <server-url> [options]
 *
 *   --folder <path>       folder to add as the library (typed into the
 *                         picker); default: the first folder the picker shows
 *   --goal <goal>         save_space | balanced | compatible | archive
 *                         (default: balanced)
 *   --timeout <seconds>   how long conversions may take (default: 300)
 *   --screenshots <dir>   save a screenshot of each step there
 *   --headed              show the browser
 *
 * The server must start with an empty data directory (first run), e.g.
 *   chrysopoeia --port 8080 --data-dir "$(mktemp -d)" --browse-root /media
 *   scripts/make-test-media.sh /media 8
 *   node web/e2e/smoke.mjs http://127.0.0.1:8080 --folder /media
 *
 * Playwright is loaded from the project if installed there, else from the
 * global npm folder. Exit code 0 means every step passed.
 */

import { execSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const GOALS = { save_space: "Save space", balanced: "Balanced", compatible: "Plays everywhere", archive: "Archive" };

function parseArgs(argv) {
  const opts = { url: null, folder: null, goal: "balanced", timeout: 300, screenshots: null, headed: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const value = () => {
      const next = argv[i + 1];
      if (next === undefined) throw new Error(`${arg} needs a value`);
      i += 1;
      return next;
    };
    if (arg === "--folder") opts.folder = value();
    else if (arg === "--goal") opts.goal = value();
    else if (arg === "--timeout") opts.timeout = Number(value());
    else if (arg === "--screenshots") opts.screenshots = value();
    else if (arg === "--headed") opts.headed = true;
    else if (arg === "-h" || arg === "--help") opts.help = true;
    else if (!opts.url && !arg.startsWith("--")) opts.url = arg;
    else throw new Error(`Unknown argument: ${arg}`);
  }
  if (!opts.help && !opts.url) throw new Error("Pass the server URL, e.g. node e2e/smoke.mjs http://127.0.0.1:8080");
  if (!(opts.goal in GOALS)) throw new Error(`--goal must be one of ${Object.keys(GOALS).join(", ")}`);
  if (!Number.isFinite(opts.timeout) || opts.timeout <= 0) throw new Error("--timeout must be a number of seconds");
  if (opts.url) opts.url = opts.url.replace(/\/+$/, "");
  return opts;
}

async function loadPlaywright() {
  try {
    return await import("playwright");
  } catch {
    const root = execSync("npm root -g", { encoding: "utf8" }).trim();
    return import(pathToFileURL(join(root, "playwright", "index.mjs")).href);
  }
}

const started = Date.now();
const log = (message) => console.log(`[smoke +${((Date.now() - started) / 1000).toFixed(1)}s] ${message}`);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

class StepError extends Error {}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.help) {
    console.log("Usage: node e2e/smoke.mjs <server-url> [--folder <path>] [--goal balanced] [--timeout 300] [--screenshots <dir>] [--headed]");
    return;
  }
  const api = async (path, init) => {
    const res = await fetch(`${opts.url}/api${path}`, init);
    const text = await res.text();
    const body = text ? JSON.parse(text) : undefined;
    if (!res.ok) throw new StepError(`${init?.method ?? "GET"} /api${path} answered ${res.status}: ${text}`);
    return body;
  };

  // 1. The server is up and on its first run.
  let health = null;
  for (let i = 0; i < 60 && !health; i += 1) {
    health = await api("/health").catch(() => null);
    if (!health) await sleep(500);
  }
  if (!health?.ok) throw new StepError(`No Chrysopoeia server answered at ${opts.url}/api/health`);
  log(`Server ${health.version} is up at ${opts.url}`);
  const [settings, libraries] = await Promise.all([api("/settings"), api("/libraries")]);
  if (settings.onboarded || libraries.length) {
    throw new StepError("The server has already been set up. Start it with an empty --data-dir for this test.");
  }

  const { chromium } = await loadPlaywright();
  const browser = await chromium.launch({ headless: !opts.headed });
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  const problems = [];
  page.on("pageerror", (err) => problems.push(`Page error: ${err.message}`));
  page.on("console", (msg) => {
    if (msg.type() !== "error") return;
    // Resource errors are covered by the responses check below.
    if (/Failed to load resource/.test(msg.text())) return;
    problems.push(`Console error: ${msg.text()}`);
  });
  page.on("response", (res) => {
    const url = new URL(res.url());
    if (!url.pathname.startsWith("/api/") || res.status() < 500) return;
    problems.push(`Server error ${res.status()} for ${res.request().method()} ${url.pathname}`);
  });
  if (opts.screenshots) mkdirSync(opts.screenshots, { recursive: true });
  let shotIndex = 0;
  const shot = async (name) => {
    if (!opts.screenshots) return;
    shotIndex += 1;
    await page.screenshot({ path: join(opts.screenshots, `${String(shotIndex).padStart(2, "0")}-${name}.png`) });
  };

  try {
    // 2. Welcome.
    await page.goto(`${opts.url}/`);
    await page.getByRole("heading", { name: /Make your video library smaller/ }).waitFor({ timeout: 20_000 });
    log("Welcome screen shown");
    await shot("welcome");

    // 3. Folder.
    await page.getByRole("button", { name: "Choose a folder" }).click();
    await page.getByRole("heading", { name: /Where are your videos\?/ }).waitFor();
    const current = page.locator("p", { hasText: /^Current folder:/ });
    const currentFolder = async () => (await current.innerText()).replace(/^Current folder:\s*/, "").trim();
    if (opts.folder) {
      const wanted = opts.folder.replace(/(.)\/+$/, "$1");
      await page.getByRole("button", { name: "Type a path" }).click();
      await page.getByRole("textbox", { name: "Folder path" }).fill(wanted);
      await page.getByRole("button", { name: "Go", exact: true }).click();
      for (let i = 0; i < 40 && (await currentFolder()) !== wanted; i += 1) await sleep(250);
      if ((await currentFolder()) !== wanted) {
        const alert = await page.getByRole("alert").allInnerTexts();
        throw new StepError(`The folder picker couldn't open ${wanted}${alert.length ? `: ${alert.join(" ")}` : ""}`);
      }
    }
    // The button names the folder it picks: Use “media” · 8 videos.
    const useFolder = page.getByRole("button", { name: /^Use “/ });
    await useFolder.waitFor();
    await page.waitForFunction(
      () => [...document.querySelectorAll("button")].some((b) => b.textContent?.trim().startsWith("Use “") && !b.disabled),
      undefined,
      { timeout: 10_000 },
    );
    const folder = await currentFolder();
    log(`Picked folder ${folder}`);
    await shot("folder");
    await useFolder.click();

    // 4. Goal, then Start.
    await page.getByRole("heading", { name: /What should happen to these videos\?/ }).waitFor();
    await page.getByRole("radio", { name: GOALS[opts.goal] }).check({ force: true });
    await shot("goal");
    await page.getByRole("button", { name: "Start" }).click();

    // 5. The app replaces setup and the library is scanning.
    await page.getByRole("navigation", { name: "Sections" }).first().waitFor({ timeout: 20_000 });
    const created = await api("/libraries");
    if (created.length !== 1) throw new StepError(`Expected one library after Start, found ${created.length}`);
    const library = created[0];
    if (library.profile.goal !== opts.goal) throw new StepError(`The library's goal is ${library.profile.goal}, not ${opts.goal}`);
    if (!(await api("/settings")).onboarded) throw new StepError("Setup finished but the server wasn't told (onboarded is false)");
    await page.getByRole("link", { name: new RegExp(library.name) }).first().waitFor();
    log(`Library "${library.name}" created with goal ${opts.goal}; overview shown`);
    await shot("overview-start");

    // 6. Conversions run; live updates reach the page without a reload.
    const deadline = Date.now() + opts.timeout * 1000;
    let sawRunning = false;
    let totals = null;
    while (Date.now() < deadline) {
      const [overview, lib] = await Promise.all([api("/overview"), api(`/libraries/${library.id}`)]);
      totals = overview.totals;
      if (overview.queue.running > 0 && !sawRunning) {
        sawRunning = true;
        const card = page.locator("article").filter({ has: page.getByRole("progressbar") }).first();
        await card.waitFor({ timeout: 15_000 });
        log("A conversion is running and its live card is on the overview");
        await shot("converting");
      }
      const busy = lib.scanning || totals.queued > 0 || totals.processing > 0 || totals.pending > 0;
      if (!busy && totals.file_count > 0) break;
      await sleep(1000);
    }
    if (!totals || totals.file_count === 0) throw new StepError("The scan found no media files in the chosen folder");
    if (totals.queued + totals.processing > 0) throw new StepError(`Conversions didn't finish within ${opts.timeout} s`);
    if (totals.done < 1) throw new StepError(`No file was converted (skipped ${totals.skipped}, failed ${totals.failed})`);
    log(`Finished: ${totals.done} converted, ${totals.skipped} skipped, ${totals.failed} failed, ${totals.saved_bytes} bytes saved`);

    if (totals.saved_bytes > 0) {
      await page.getByText("saved", { exact: true }).first().waitFor({ timeout: 15_000 });
      log("The overview shows the space saved without a reload");
    }
    await shot("overview-done");

    // 7. History and a job's verification report.
    await page.getByRole("link", { name: "Queue", exact: true }).first().click();
    await page.getByRole("link", { name: /^History/ }).click();
    const verified = page.getByRole("button", { name: /Verified/ }).first();
    await verified.waitFor({ timeout: 15_000 });
    await verified.click();
    const sheet = page.getByRole("dialog");
    await sheet.getByRole("heading", { name: "Checks" }).waitFor();
    await sheet.getByText(/Verified and (replaced|saved)/).waitFor();
    log("The job sheet shows a verified result with its checks");
    await shot("job-sheet");
    await page.keyboard.press("Escape");

    // 8. Hardware page reads without errors.
    await page.goto(`${opts.url}/#/settings/hardware`);
    await page.getByRole("heading", { name: "This machine" }).waitFor({ timeout: 20_000 });
    const failedCallouts = await page.getByText("ffmpeg wasn't found").count();
    if (failedCallouts) throw new StepError("The hardware page says ffmpeg wasn't found");
    log("Hardware page loaded");
    await shot("hardware");

    if (problems.length) throw new StepError(`The page reported problems:\n  ${problems.join("\n  ")}`);
    log("All steps passed");
  } catch (err) {
    await shot("failure").catch(() => undefined);
    throw err;
  } finally {
    await browser.close();
  }
}

main().catch((err) => {
  console.error(`[smoke] FAILED: ${err instanceof StepError ? err.message : err?.stack ?? err}`);
  process.exit(1);
});
