# Railway deploy — config-as-code home

Railway builds the root `Dockerfile` from `main`: a cargo-chef build of this fork
that bakes `deploy/railway/relay.yaml` into the image and boots
`relay --config /app/relay.yaml --config-only`. Railway has no file mounts and
chain/asset maps are impractical as env vars, so the config ships in the image.
**No secrets ever in `relay.yaml`.** Secrets (`RELAY_MNEMONIC`,
`RELAY_FUNDER_SIGNER_KEY`, `RELAY_FUNDER_OWNER_KEY`, `RELAY_DB_URL`, `GECKO_API`,
keyed `RPC_<chainId>` URLs) live in Railway env only and are read at boot.

`relay.yaml` carries chain blocks and contract addresses, the sponsorship policy,
pricefeed remaps, the Better Auth JWKS URL, and the passkey association files
served under `/.well-known/`. Any change to it needs an image rebuild — push to
`main` and Railway redeploys.

The fork pins upstream `ithacaxyz/relay` at `v26.1.4` (see `Cargo.toml`) but
diverges on top of it: gas sponsorship, user-gated quota via Better Auth JWTs,
the `/.well-known/` passkey endpoints, boot diagnostics. Upstream syncs merge
into `main` like any other branch. `deploy/railway/Dockerfile` (a wrapper around
the stock upstream image) predates the fork build and is not what production
runs — the stock image cannot serve fork-only endpoints.

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
