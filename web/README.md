# Chrysopoeia web UI

The browser interface for Chrysopoeia. It is a Next.js app exported as static
files (`web/out`) that the Rust server hosts next to the API, so the container
needs no Node at runtime. Everything talks to `/api` on the same origin; see
`docs/ARCHITECTURE.md` ("REST API", "WebSocket", "Web UI") for the contract.

## Scripts

| Command | What it does |
|---|---|
| `pnpm install` | Install dependencies (fonts are self-hosted via `@fontsource`, no Google access needed) |
| `pnpm dev` | Dev server on http://localhost:3000 |
| `pnpm build` | Static export to `web/out` (`out/index.html` plus `_next/` assets) |
| `pnpm lint` | ESLint |
| `pnpm typecheck` | `tsc --noEmit` |
| `pnpm test` | Unit and component tests (Vitest + jsdom): API errors and their `field`, live events and reconnecting, router and navigation guard, profiles, formatting |
| `pnpm e2e <url>` | Browser smoke test of the first-run flow against a running server with an empty data dir (`e2e/smoke.mjs`, needs Playwright; see the file for options) |
| `pnpm mock` | Dev-only mock API with fake sample data on http://localhost:8787 |

## Working on the UI

Against a real server (start it with `--dev-cors` so the browser may call it
from another port):

```sh
NEXT_PUBLIC_API_URL=http://localhost:8080 pnpm dev
```

Without a server, run the mock API in another terminal:

```sh
pnpm mock                        # a populated demo library
MOCK_SCENARIO=fresh pnpm mock    # first run: shows the setup flow
MOCK_SCENARIO=nogpu pnpm mock    # no GPU, with setup hints
MOCK_SCENARIO=empty pnpm mock    # set up, but no libraries yet
MOCK_DETECT_MS=8000 pnpm mock    # "Checking your hardware…" for the first 8 s
MOCK_WS=off pnpm mock            # no WebSocket, like a proxy without it: the app polls
MOCK_MAX_JOBS=2 pnpm mock        # the job limit comes from the container's MAX_JOBS
MOCK_FORCE=reject pnpm mock      # an older server refuses "Convert anyway" (=ignore: accepts it, still skips)
MOCK_HOST=deny pnpm mock         # every request answers 403 host_not_allowed
MOCK_SETTLE_MS=0 pnpm mock       # files still being copied never settle (default: after 60 s)
NEXT_PUBLIC_API_URL=http://localhost:8787 pnpm dev
```

`scripts/mock-api.mjs` is never imported by the app and is not part of the
exported bundle. It follows the real server's error codes, messages and
`field`s (nested ones such as `profile.max_height` too), serves `/api/system`,
`Job.notes`, `HardwareInfo.detecting` and the round-3 additions
(`max_jobs_source`, `settling`, `build`, HDR10 metadata, `force`, `left_out`,
capped folder counts), and uses the server's own sentences for damaged
originals; keep it in step when the API changes. `NEXT_PUBLIC_API_URL` is baked in at build time; production
builds leave it unset so the UI uses the same origin.

## End-to-end smoke test

```sh
scripts/make-test-media.sh /tmp/media 8          # from the repository root
target/release/chrysopoeia --port 8080 --data-dir "$(mktemp -d)" \
  --web-dir web/out --browse-root /tmp/media &
cd web && pnpm e2e http://127.0.0.1:8080 --folder /tmp/media --screenshots /tmp/smoke
```

It walks welcome → folder → goal → Start, waits for the scan and the
conversions, checks that the overview shows the space saved without a
reload, opens a verified job's checks and the hardware page, and fails on
page errors or server errors.

## Layout

- `src/app` — root layout (fonts, theme boot script) and the single page.
- `src/components/app.tsx` — picks setup, "can't reach the server" or the app,
  and maps hash routes (`#/`, `#/queue`, `#/library/<id>`, `#/settings/hardware`,
  …) to screens. Detail sheets open from `?job=<id>` and `?file=<id>`.
- `src/screens` — one file per screen.
- `src/components` — shared pieces; `ui/` holds the primitives.
- `src/lib/types.ts` — mirror of `crates/chrysopoeia-core` (keep in sync).
- `src/lib/api.ts` — REST client and `ApiError`.
- `src/lib/live.ts` — WebSocket client that patches the query cache.
- `src/lib/format.ts`, `labels.ts` — plain-language wording and number formats.
