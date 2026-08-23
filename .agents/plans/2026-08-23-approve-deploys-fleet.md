# Approve in skiff triggers a full fleet deploy

Date: 2026-08-23 · Card: #121 · Scope: tugboat serve (Rust), skiff bridge (Node), skiff app (Rails)

## Goal

"Approve" in the skiff desk means **land and ship**: the human's approval pushes
the change to `origin/main` (exactly as today) and then triggers a full fleet
deploy of every deployable service — unconditionally, no scope logic.

## The rule

Approve = land + full fleet deploy. Every approval deploys every discoverable
deployable (any top-level dir with a `deploy.toml`), in manifest order, each
with its own atomic install → health-check → rollback, continue-on-error.
Native-app-only and docs-only changes deploy the fleet too — one rule, no
exceptions. Absent `TUGBOAT_SERVE_TOKEN` in the bridge env = today's behavior
(feature disabled).

## Design

### tugboat serve (round 1)

- `POST /deploy` — deploy everything: reserve each service's in-flight slot and
  spawn one job per deployable, in manifest order; response
  `{jobs: [{name, job_id} | {name, status: "in_progress"}]}`. A service already
  deploying is reported as `in_progress`, never blocks the rest.
- `GET /jobs/{id}` — `{id, outcome: null | {ok, error}}` — the poll-friendly
  terminal outcome; the live transcript stays on `/jobs/{id}/stream`.
- Refactor: the per-service job start moves into `start_deploy_job`, shared by
  `/deploy/{name}` and `/deploy`; the wire mapping `fleet_entry` is pure and
  unit-tested.

### skiff bridge (round 2)

- New `lib/tugboat.js` client: `deployAll()` + `jobStatus(id)`; URL
  `TUGBOAT_SERVE_URL` (default `http://127.0.0.1:7878`), token
  `TUGBOAT_SERVE_TOKEN` (absent → disabled).
- `land()` after the push: trigger the fleet deploy, record a `deploy` event on
  the change (services + job ids), poll outcomes (bounded), append results.
  Recorded, never un-ships.
- `change-store.js`: new additive `deploy` event type (replay skips unknown
  types, so old logs degrade cleanly).
- `enrich()`: `willDeploy` = deployable count from `/services` (cached) or
  `null` when the daemon is unreachable.

### skiff app (round 3)

- Review page: "Approval will deploy the whole fleet (N services)" preview +
  deploy outcome readout (DW-001).

## Rollout (after the change lands)

1. `tugboat self-deploy` on the desktop — the daemon must run the new endpoints.
2. Restart `skiff-bridge.service`.
3. Append `TUGBOAT_SERVE_TOKEN` (copied from `~/.config/tugboat/serve.env`,
   never echoed) and `TUGBOAT_SERVE_URL` to `~/.config/skiff/secrets` — the
   bridge wrapper exports every KEY=VALUE there to the bridge process already.

## Gates

`cargo test --workspace` · `cargo clippy --workspace --all-targets -- -D warnings` ·
`tugboat fleet gen --check` · round 2 adds `node --test bridge/test/*.test.js` ·
round 3 adds `bin/ci` from `skiff/`.

## Edges

- New service in a change: `provision.sh` is manual and tugboat never runs it —
  a never-provisioned service fails that one deploy, the rest proceed, rollback
  protects.
- A change touching tugboat itself is not a deployable → auto-deploys nothing;
  the daemon self-updates via `tugboat self-deploy` separately.
- Daemon down at approve time → deploy recorded as unavailable on the change;
  the change stays shipped; redeploy via lighthouse.
- A failed deploy job → outcome recorded on the change; the change stays shipped
  (landed is landed; the deploy is operational).
