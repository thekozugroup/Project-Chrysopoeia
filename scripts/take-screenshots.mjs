#!/usr/bin/env node
/**
 * Takes the screenshots in docs/screenshots from a running Szalinski server,
 * the way a new user would meet it: dark theme, 1440x900, real conversions of
 * the demo library (scripts/make-demo-media.sh).
 *
 * Usage:
 *   node scripts/take-screenshots.mjs <server-url> [options]
 *
 *   --out <dir>        where to write the images (default docs/screenshots)
 *   --movies <path>    folder for the first library, chosen in the first-run
 *                      screens (default /media/Movies)
 *   --tv <path>        folder for a second library, added through the API
 *                      (default /media/TV; "none" to skip it)
 *   --timeout <secs>   how long to wait for conversions (default 1200)
 *
 * The server must be on its first run (an empty data folder) with the demo
 * library mounted, for example:
 *
 *   scripts/make-demo-media.sh /srv/demo-media
 *   docker run -d -p 8080:8080 -v /srv/demo-media:/media -v "$(mktemp -d)":/config \
 *     -e PUID=$(id -u) -e PGID=$(id -g) szalinski:dev
 *   node scripts/take-screenshots.mjs http://127.0.0.1:8080
 *
 * Writes setup.png (first-run goal step), overview.png and queue.png (a
 * conversion running, savings so far), job.png (a finished file's checks),
 * hardware.png (Settings > Hardware, "This machine") and phone.png (the
 * overview on a 390x844 phone, 2x). Shrink them afterwards, e.g.
 * `pngquant --force --ext .png --quality 60-85 docs/screenshots/*.png`.
 *
 * Playwright is loaded from the project if installed there, else from the
 * global npm folder.
 */

import { execSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

function parseArgs(argv) {
  const opts = { url: null, out: "docs/screenshots", movies: "/media/Movies", tv: "/media/TV", timeout: 1200 };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const value = () => {
      const next = argv[i + 1];
      if (next === undefined) throw new Error(`${arg} needs a value`);
      i += 1;
      return next;
    };
    if (arg === "--out") opts.out = value();
    else if (arg === "--movies") opts.movies = value();
    else if (arg === "--tv") opts.tv = value();
    else if (arg === "--timeout") opts.timeout = Number(value());
    else if (arg === "-h" || arg === "--help") opts.help = true;
    else if (!opts.url && !arg.startsWith("--")) opts.url = arg.replace(/\/+$/, "");
    else throw new Error(`Unknown argument: ${arg}`);
  }
  if (!opts.help && !opts.url) throw new Error("Pass the server URL, e.g. node scripts/take-screenshots.mjs http://127.0.0.1:8080");
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
const log = (message) => console.log(`[screenshots +${((Date.now() - started) / 1000).toFixed(0)}s] ${message}`);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (opts.help) {
    console.log("Usage: node scripts/take-screenshots.mjs <server-url> [--out docs/screenshots] [--movies /media/Movies] [--tv /media/TV|none] [--timeout 1200]");
    return;
  }
  mkdirSync(opts.out, { recursive: true });
  const api = async (path, init) => {
    const res = await fetch(`${opts.url}/api${path}`, init);
    const text = await res.text();
    if (!res.ok) throw new Error(`${init?.method ?? "GET"} /api${path} answered ${res.status}: ${text}`);
    return text ? JSON.parse(text) : undefined;
  };

  let health = null;
  for (let i = 0; i < 60 && !health; i += 1) {
    health = await api("/health").catch(() => null);
    if (!health) await sleep(500);
  }
  if (!health?.ok) throw new Error(`No Szalinski server answered at ${opts.url}/api/health`);
  const [settings, libraries] = await Promise.all([api("/settings"), api("/libraries")]);
  if (settings.onboarded || libraries.length) throw new Error("The server has already been set up. Start it with an empty data folder.");

  const { chromium } = await loadPlaywright();
  const browser = await chromium.launch();
  const shot = async (page, name, options = {}) => {
    await sleep(700); // let transitions and live numbers settle
    await page.screenshot({ path: join(opts.out, `${name}.png`), animations: "disabled", ...options });
    log(`Saved ${name}.png`);
  };
  try {
    const context = await browser.newContext({
      viewport: { width: 1440, height: 900 },
      colorScheme: "dark",
      reducedMotion: "reduce",
      locale: "en-US",
    });
    const page = await context.newPage();

    // First run: welcome, folder, goal.
    await page.goto(`${opts.url}/`);
    await page.getByRole("heading", { name: /Make your video library smaller/ }).waitFor({ timeout: 20_000 });
    // The welcome page names the GPU or says the CPU will do the work once detection is done.
    await page.getByText(/Found |No GPU found|Checking your hardware/).first().waitFor();
    await page.waitForFunction(() => !document.body.innerText.includes("Checking your hardware"), undefined, { timeout: 60_000 });
    await page.getByRole("button", { name: "Choose a folder" }).click();
    await page.getByRole("heading", { name: /Where are your videos\?/ }).waitFor();
    await page.getByRole("button", { name: "Type a path" }).click();
    await page.getByRole("textbox", { name: "Folder path" }).fill(opts.movies);
    await page.getByRole("button", { name: "Go", exact: true }).click();
    const useFolder = page.getByRole("button", { name: /^Use “/ });
    await useFolder.waitFor();
    await page.waitForFunction(
      () => [...document.querySelectorAll("button")].some((b) => b.textContent?.trim().startsWith("Use “") && !b.disabled),
      undefined,
      { timeout: 15_000 },
    );
    await useFolder.click();
    await page.getByRole("heading", { name: /What should happen to these videos\?/ }).waitFor();
    // The hardware's own suggestion (marked "Best fit") stays selected.
    await page.getByText(/Fast|Medium|Slow here/).first().waitFor();
    await shot(page, "setup");
    await page.getByRole("button", { name: "Start" }).click();
    await page.getByRole("navigation", { name: "Sections" }).first().waitFor({ timeout: 20_000 });
    log("First run done; the first library is scanning");

    if (opts.tv !== "none") {
      await api("/libraries", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ path: opts.tv, name: "TV", goal: "save_space" }),
      });
    }

    // Wait for a conversion that is part-way, with some already done.
    const deadline = Date.now() + opts.timeout * 1000;
    let captured = false;
    while (Date.now() < deadline) {
      const [overview, running] = await Promise.all([api("/overview"), api("/jobs?state=running&limit=5")]);
      const job = running.items.find((j) => j.stage === "transcoding" && j.progress >= 30 && j.progress <= 65);
      // Files that were just replaced are shown as "waiting to finish copying" for a
      // few seconds; capture once that has passed.
      if (job && overview.totals.done >= 3 && overview.totals.settling === 0) {
        await page.goto(`${opts.url}/#/`);
        await page.getByText("Converting now").waitFor({ timeout: 15_000 });
        await shot(page, "overview");
        await page.goto(`${opts.url}/#/queue`);
        await page.getByRole("progressbar").first().waitFor({ timeout: 15_000 });
        await shot(page, "queue");
        const phone = await browser.newContext({
          viewport: { width: 390, height: 844 },
          deviceScaleFactor: 2,
          isMobile: true,
          hasTouch: true,
          colorScheme: "dark",
          reducedMotion: "reduce",
          locale: "en-US",
        });
        const phonePage = await phone.newPage();
        await phonePage.goto(`${opts.url}/#/`);
        await phonePage.getByText("Converting now").waitFor({ timeout: 15_000 });
        await shot(phonePage, "phone");
        await phone.close();
        captured = true;
        break;
      }
      await sleep(1000);
    }
    if (!captured) throw new Error("No conversion was part-way with files already done before the timeout");

    // Everything finished: a verified file's checks.
    while (Date.now() < deadline) {
      const queue = await api("/queue");
      const libs = await api("/libraries");
      if (queue.running === 0 && queue.queued === 0 && libs.every((l) => !l.scanning)) break;
      await sleep(2000);
    }
    const history = await api("/jobs?state=history&limit=50");
    const done = history.items.filter((j) => j.state === "done" && j.validation?.passed);
    if (!done.length) throw new Error("No verified conversion to show");
    // The biggest saving reads best.
    done.sort((a, b) => (b.input_size ?? 0) - (a.input_size ?? 0));
    const pick = done[0];
    await page.goto(`${opts.url}/#/queue/history`);
    await page.getByRole("button", { name: new RegExp(pick.file_name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")) }).first().click();
    const sheet = page.getByRole("dialog");
    await sheet.getByRole("heading", { name: "Checks" }).waitFor();
    await sheet.getByRole("heading", { name: "Checks" }).scrollIntoViewIfNeeded();
    await shot(page, "job");
    await page.keyboard.press("Escape");

    await page.goto(`${opts.url}/#/settings/hardware`);
    await page.getByRole("heading", { name: "This machine" }).waitFor({ timeout: 20_000 });
    await page.waitForFunction(() => !document.body.innerText.includes("Checking your hardware"), undefined, { timeout: 60_000 });
    await shot(page, "hardware");
    await context.close();
  } finally {
    await browser.close();
  }
}

main().catch((err) => {
  console.error(`[screenshots] FAILED: ${err?.stack ?? err}`);
  process.exit(1);
});
