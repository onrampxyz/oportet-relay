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
`--config-only` ignores CLI overrides). No scale-to-zero.

**`PORT=9119` must exist as a service variable even though the relay ignores it.**
The healthcheck (`railway.toml` → `/ready`) probes a port Railway picks itself, and
the `targetPort: 9119` on the domains does not feed that choice — with no `PORT` set
it probes the wrong one and every attempt fails with "service unavailable" while the
public URL serves fine. Symptom to recognise: the container logs `Started relay
service`, the path answers 200 from outside, and the deploy still fails 11/11.
Setting `PORT=9119` made the same image pass on attempt 1.

`server.address` is `::` so the service is reachable on `relay.railway.internal`
(Railway private networking is IPv6). That is separate from the healthcheck.

Execution plan: monorepo `.planning/research/porto-relay/exec-plan-railway-and-contracts.md`.
