# Railway deploy — config-as-code home

This fork does NOT diverge relay code. `main` mirrors `ithacaxyz/relay` (pin: `v26.1.4`);
this `deploy/railway` branch carries our deploy configuration only, so upstream syncs
never conflict with it.

- `Dockerfile` — wraps the stock upstream image, bakes `relay.yaml` in (Railway has no
  file mounts; chain/asset maps are impractical as env vars).
- `relay.yaml` — chain blocks + contract addresses (authored in Railway-plan Slices 1-2).
  **No secrets ever.** Secrets (`RELAY_MNEMONIC`, `RELAY_FUNDER_SIGNER_KEY`, `GECKO_API`,
  `RELAY_DB_URL`, keyed RPC URLs) live in Railway env only.

Port is pinned to 9119 (relay binds the config port, not Railway's `$PORT`;
`--config-only` ignores CLI overrides). Healthcheck: `/ready`. No scale-to-zero.

Execution plan: monorepo `.planning/research/porto-relay/exec-plan-railway-and-contracts.md`.
