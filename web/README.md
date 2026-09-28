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
| `pnpm test` | Unit and component tests (Vitest + jsdom): live events, router and navigation guard, profiles, formatting |
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
NEXT_PUBLIC_API_URL=http://localhost:8787 pnpm dev
```

`scripts/mock-api.mjs` is never imported by the app and is not part of the
exported bundle. `NEXT_PUBLIC_API_URL` is baked in at build time; production
builds leave it unset so the UI uses the same origin.

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
