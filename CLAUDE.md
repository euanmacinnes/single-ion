# single-ion — Claude Code Guidelines

## Overview

**single-ion** is a convenience wrapper that runs all four Free Radicals services — Reactive,
ION, Gluon, and Neutrino — as concurrent Tokio tasks inside a single process. It is the
**primary local development binary** for the full stack.

## Commands

```bash
cd single-ion
cargo check
cargo fmt && cargo clippy -- -D warnings
cargo run --bin single-ion        # full stack on default ports
```

| Service   | Port  | URL                        |
|-----------|-------|----------------------------|
| ION       | 8080  | http://localhost:8080      |
| Reactive  | 7878  | http://localhost:7878      |
| pgwire    | 5433  | postgresql://localhost:5433 |
| Gluon     | 4747  | ws://localhost:4747        |
| Neutrino  | 4748  | http://localhost:4748      |

## Component development workflow

single-ion is the **canonical way to test ION Svelte components**. Never use Vite dev servers
or `preview_start` for integration testing.

1. Build the component: `cd ion/static/<component> && npm run build`
2. Start the full stack: `cd single-ion && cargo run --bin single-ion`
3. Open the dev page: `http://localhost:8080/dev/<component-name>`

The `/dev/` menu at `http://localhost:8080/dev` lists every available test page. Each page
exercises one component against the live Reactive backend with real auth and WebSocket support.

See `ion/CLAUDE.md` → "Svelte UI Components — Dev & Build" for how to add a new dev page.

## Config

`cfg/single-ion.yaml` (or `ION_*` / `REACTIVE_*` env vars) override any per-service defaults.
See `docs/user guide.md` for full configuration reference.

## Telemetry

single-ion does not own a telemetry pipeline of its own — every embedded service
(ION, Reactive, Gluon, Neutrino) keeps its own publisher and writes to its own
`<service>/telemetry` topic exactly as it does in standalone mode. Because all four services
share one Gluon instance in-process, the timeline ring buffer in gluon admin already shows the
unified cross-service stream.

For local debugging this means:

- The **gluon admin Slow tab** at `http://localhost:4747/admin` (Slow) is the fastest way to
  find the slowest user actions across the whole stack. Click a row → trace detail → see
  every span emitted under that `trace_id` in time order, regardless of which service
  emitted it.
- Per-request diagnostic events follow the wire format and naming conventions in root
  `CLAUDE.md` "Diagnostic Telemetry — Per-Request Traces". Don't add a single-ion-specific
  schema — the unified pipeline is the whole point of running the services in one process.

When reporting "this is slow" against single-ion, paste the trace detail (not screenshots) so
the per-stage span breakdown is preserved.
