#!/usr/bin/env node
/**
 * Phone and tablet layout check: the built UI (`web/out`, from `pnpm build`)
 * against the mock API with every file given a long, Plex/Sonarr-style name
 * (about 90 characters). It fails when a screen scrolls sideways, when a
 * running job's Stop or Details button is off screen, when those buttons
 * aren't named after their file, when a name's late episode is cut off, when
 * a stacked quality option on a touch screen is shorter than the 24 px
 * WCAG 2.2 target size, or when a confirmation's title (Stop, with a dotted
 * release name that has no place to break) runs past its dialog.
 *
 * Usage (from web/):
 *   pnpm build && node e2e/layout.mjs [--port 18925] [--screenshots <dir>] [--headed]
 *
 * It serves `out/` itself on --port and runs the mock API on --port + 1.
 * Playwright is loaded from the project if installed there, else from the
 * global npm folder. Exit code 0 means every check passed.
 */

import { execSync, spawn } from "node:child_process";
import { createReadStream, existsSync, mkdirSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, extname, join, normalize } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const WEB = join(dirname(fileURLToPath(import.meta.url)), "..");
const OUT = join(WEB, "out");

function parseArgs(argv) {
  const opts = { port: 18925, screenshots: null, headed: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--port") opts.port = Number(argv[++i]);
    else if (arg === "--screenshots") opts.screenshots = argv[++i];
    else if (arg === "--headed") opts.headed = true;
    else throw new Error(`Unknown argument: ${arg}`);
  }
  if (!Number.isInteger(opts.port) || opts.port <= 0) throw new Error("--port must be a port number");
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

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript",
  ".css": "text/css",
  ".svg": "image/svg+xml",
  ".woff2": "font/woff2",
  ".json": "application/json",
  ".txt": "text/plain",
};

/** A static server for `out/`; `/api` is answered in the browser (see `route`). */
function serveOut(port) {
  const server = createServer((req, res) => {
    const path = decodeURIComponent(new URL(req.url, "http://x").pathname);
    let file = normalize(join(OUT, path === "/" ? "index.html" : path));
    if (!file.startsWith(OUT) || !existsSync(file) || statSync(file).isDirectory()) file = join(OUT, "index.html");
    res.writeHead(200, { "Content-Type": TYPES[extname(file)] ?? "application/octet-stream" });
    createReadStream(file).pipe(res);
  });
  return new Promise((resolve) => server.listen(port, "127.0.0.1", () => resolve(server)));
}

const EPISODE = /S\d{2}E\d{2}/;

/**
 * A long name in the style media servers use (about 90 characters): an
 * episode far into a long series title ("Das außergewöhnlich lange … -
 * S01E04 - …"), or a movie with a long release suffix.
 */
function longName(name) {
  const ext = /\.[^.]+$/.exec(name)?.[0] ?? "";
  const episode = EPISODE.exec(name)?.[0];
  if (episode) return `Das außergewöhnlich lange Serienfinale einer Show (2024) - ${episode} - Extended Cut Bluray-1080p${ext}`;
  const base = name.slice(0, name.length - ext.length);
  return `${base} (Director's Cut, Remastered) Bluray-2160p Remux HDR10 DTS-HD MA 7.1${ext}`;
}

/** A scene-release name: no space anywhere, so nothing lets a title break. */
const DOTTED = "Some.Really.Long.Scene.Release.Name.2021.1080p.BluRay.x264.DTS-HD.MA.5.1.REMUX-GROUP.mkv";
/** While set, running jobs are named `DOTTED` (for the confirmation title check). */
let dotted = false;

/** Give every `file_name` in an API answer a long name. */
function lengthen(value) {
  if (Array.isArray(value)) return value.map(lengthen);
  if (value && typeof value === "object") {
    const out = {};
    for (const [key, v] of Object.entries(value)) {
      if (key === "file_name" && typeof v === "string") out[key] = dotted && value.state === "running" ? DOTTED : longName(v);
      else out[key] = lengthen(v);
    }
    return out;
  }
  return value;
}

const failures = [];
const fail = (message) => {
  failures.push(message);
  console.log(`  FAIL ${message}`);
};

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  if (!existsSync(join(OUT, "index.html"))) throw new Error("web/out is missing: run pnpm build first");
  const mockPort = opts.port + 1;
  const mock = spawn(process.execPath, [join(WEB, "scripts", "mock-api.mjs")], {
    env: { ...process.env, PORT: String(mockPort), MOCK_WS: "off" },
    stdio: ["ignore", "pipe", "inherit"],
  });
  await new Promise((resolve, reject) => {
    mock.stdout.on("data", (chunk) => String(chunk).includes("mock API") && resolve());
    mock.on("exit", (code) => reject(new Error(`the mock API stopped (${code})`)));
  });
  const server = await serveOut(opts.port);
  const { chromium } = await loadPlaywright();
  const browser = await chromium.launch({ headless: !opts.headed });
  const base = `http://127.0.0.1:${opts.port}`;
  if (opts.screenshots) mkdirSync(opts.screenshots, { recursive: true });

  try {
    const libraries = await (await fetch(`http://127.0.0.1:${mockPort}/api/libraries`)).json();
    const screens = ["#/", "#/queue", "#/queue/next", "#/queue/history", `#/library/${libraries[0].id}`];
    const viewports = [
      { name: "phone", width: 390, height: 844, isMobile: false },
      // The narrowest screens still in use: the Overview's heading and rows must wrap, not run past the edge.
      { name: "phone-small", width: 320, height: 640, isMobile: false },
      { name: "tablet", width: 768, height: 1024, isMobile: false },
      // A real phone: an overflowing page would widen the layout viewport instead of scrolling.
      { name: "phone-touch", width: 390, height: 844, isMobile: true, hasTouch: true },
    ];
    for (const vp of viewports) {
      const context = await browser.newContext({
        viewport: { width: vp.width, height: vp.height },
        isMobile: vp.isMobile,
        hasTouch: Boolean(vp.hasTouch),
      });
      await context.route("**/api/**", async (route) => {
        const url = new URL(route.request().url());
        const response = await route.fetch({ url: `http://127.0.0.1:${mockPort}${url.pathname}${url.search}` });
        const type = response.headers()["content-type"] ?? "";
        if (!type.includes("json")) return route.fulfill({ response });
        return route.fulfill({ response, json: lengthen(await response.json()) });
      });
      const page = await context.newPage();
      page.on("pageerror", (err) => fail(`${vp.name}: page error ${err.message}`));
      for (const screen of screens) {
        const where = `${vp.name} ${screen}`;
        await page.goto(`${base}/${screen}`);
        await page.locator("main h1").first().waitFor({ timeout: 15_000 });
        if (screen === "#/queue" || screen === "#/") {
          await page.locator("article").first().waitFor({ timeout: 15_000 });
        }
        await page.waitForTimeout(400);
        const widths = await page.evaluate(() => ({
          scroll: document.documentElement.scrollWidth,
          inner: window.innerWidth,
        }));
        console.log(`${where}: scrollWidth ${widths.scroll}, innerWidth ${widths.inner}`);
        if (widths.scroll > widths.inner) fail(`${where} scrolls sideways (${widths.scroll} > ${widths.inner})`);
        if (widths.inner !== vp.width) fail(`${where} widened the layout viewport to ${widths.inner}`);
        // A late episode stays in view however the name is cut (its first six characters aren't clipped).
        const clipped = await page.locator("[data-file-tail]").evaluateAll((tails) =>
          tails
            .filter((t) => t.getBoundingClientRect().width > 0 && t.firstChild)
            .filter((t) => {
              const range = document.createRange();
              range.setStart(t.firstChild, 0);
              range.setEnd(t.firstChild, Math.min(6, t.firstChild.length));
              return range.getBoundingClientRect().right > t.getBoundingClientRect().right + 0.5;
            })
            .map((t) => t.textContent),
        );
        if (clipped.length) fail(`${where}: the episode is cut off in ${clipped.length} name(s), e.g. "${clipped[0]}"`);
        if (screen === "#/queue") {
          const cards = await page.locator("article").evaluateAll((articles) =>
            articles.map((a) =>
              [...a.querySelectorAll("button")]
                .filter((b) => /^(Stop|Details)$/.test(b.textContent?.trim() ?? ""))
                .map((b) => ({ text: b.textContent?.trim(), label: b.getAttribute("aria-label"), right: b.getBoundingClientRect().right })),
            ),
          );
          const names = new Set();
          for (const buttons of cards) {
            if (buttons.length < 2) fail(`${where}: a running card lacks its Stop and Details buttons`);
            for (const b of buttons) {
              if (b.right > widths.inner) fail(`${where}: ${b.text} is off screen (right edge ${Math.round(b.right)})`);
              if (!b.label || !b.label.startsWith(b.text) || b.label === b.text) fail(`${where}: ${b.text} isn't named after its file`);
              names.add(b.label);
            }
          }
          if (names.size !== cards.flat().length) fail(`${where}: card buttons share accessible names`);
        }
        if (opts.screenshots) {
          await page.screenshot({ path: join(opts.screenshots, `${vp.name}-${screen.replace(/[#/]+/g, "_") || "overview"}.png`) });
        }
      }
      {
        // A confirmation names the file in its title. A dotted release name has nowhere to break, so the
        // title must wrap anywhere instead of running past the dialog.
        dotted = true;
        await page.goto(`${base}/#/queue`);
        await page.locator("main h1").first().waitFor({ timeout: 15_000 });
        const stop = page.locator("article").first().getByRole("button", { name: /^Stop / });
        await stop.waitFor({ timeout: 15_000 });
        await stop.click();
        const dialog = page.getByRole("alertdialog");
        await dialog.waitFor({ timeout: 10_000 });
        await page.waitForTimeout(400);
        const fit = await dialog.evaluate((el) => {
          const title = el.querySelector("h2") ?? el.querySelector("[id]");
          let farthest = 0;
          for (const e of el.querySelectorAll("*")) farthest = Math.max(farthest, e.getBoundingClientRect().right);
          return {
            scroll: el.scrollWidth,
            client: el.clientWidth,
            titleScroll: title?.scrollWidth ?? 0,
            titleClient: title?.clientWidth ?? 0,
            farthest,
            right: el.getBoundingClientRect().right,
          };
        });
        console.log(`${vp.name} Stop dialog (dotted name): dialog ${fit.scroll}/${fit.client}, title ${fit.titleScroll}/${fit.titleClient}`);
        if (fit.scroll > fit.client) fail(`${vp.name}: the Stop dialog scrolls sideways (${fit.scroll} > ${fit.client})`);
        if (fit.titleScroll > fit.titleClient) fail(`${vp.name}: the Stop dialog's title runs past its box (${fit.titleScroll} > ${fit.titleClient})`);
        if (fit.farthest > fit.right + 0.5) fail(`${vp.name}: something in the Stop dialog runs past its edge`);
        if (opts.screenshots) await page.screenshot({ path: join(opts.screenshots, `${vp.name}-stop-dialog.png`) });
        await page.keyboard.press("Escape");
        dotted = false;
      }
      if (vp.hasTouch) {
        // Stacked quality options stay touch-sized (WCAG 2.2 target size, 24 px; 44 px intended).
        await page.goto(`${base}/#/settings/advanced`);
        const quality = page.getByRole("radiogroup", { name: "Quality" });
        await quality.waitFor({ timeout: 15_000 });
        const heights = await quality.locator("label").evaluateAll((labels) => labels.map((l) => l.getBoundingClientRect().height));
        console.log(`${vp.name} #/settings/advanced: quality option heights ${heights.map(Math.round).join(", ")}`);
        if (heights.some((h) => h < 24)) fail(`${vp.name}: a quality option is under 24 px tall`);
        if (opts.screenshots) await page.screenshot({ path: join(opts.screenshots, `${vp.name}-settings-advanced.png`) });
      }
      await context.close();
    }
  } finally {
    await browser.close();
    server.closeAllConnections();
    server.close();
    mock.kill();
  }
  if (failures.length) {
    console.log(`\n${failures.length} layout check(s) failed.`);
    process.exit(1);
  }
  console.log("\nEvery layout check passed.");
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
