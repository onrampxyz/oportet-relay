# LayerZero Settler Durability — Packet-Loss Risk & Hardening

**Status:** Parked design note (2026-07-13). Implement the hardening below **when LayerZero interop is enabled for mainnet cross-chain** (Base ↔ Rise). Same-chain gas-sponsored sends do not touch the settler and are unaffected by everything here.

**Why this exists:** enabling LayerZero cross-chain settlement adds a websocket-log-subscription surface whose failure modes are non-obvious. This note captures the analysis so the durability work is not re-derived (or forgotten) when we turn interop on.

---

## TL;DR — safety vs availability

- **Funds are never at risk of loss.** No packet-loss path strands escrowed funds. The escrow refund loop guarantees the user gets their assets back if a settlement never completes.
- The realistic failure mode is **degraded availability**: a flaky or silently-dead websocket downgrades a cross-chain send from "delivered" to "timeout → auto-refund." The user is made whole, but the cross-chain delivery didn't happen and they must retry.
- Two cheap local changes close the main gap: a **periodic poll tick** and a **separate http read provider**. With both, the settler stays correct through a total websocket outage by falling back to pure polling.
- These files are upstream ithaca code; the hardening is a small patch on our fork.

---

## How verification actually works (hint vs. truth)

Core loop: `src/interop/settler/layerzero/verification.rs`, `monitor_packet` (line 245).

The websocket `PayloadVerified` log is treated as a **trigger, not a source of truth**. Authoritative truth is an on-chain read, `is_message_available`, which is monotonic (once a packet is verified it stays verified).

Sequence per packet:

1. **Subscribe first** (`subscribe_to_payload_events`, line 251 → 336 `provider.subscribe_logs(...)`), before reading chain state — so no event can slip through the gap between "check" and "subscribe".
2. **Immediately read chain state** `is_message_available` (line 254). Catches packets already verified before we subscribed.
3. On each received event, **re-read chain state** (line 267) rather than trusting the event. Multiple DVNs emit `PayloadVerified`; we don't track DVN quorum, we just re-poll availability.
4. On stream `Err` (ws drop, `broadcast` lag) the inner loop breaks (line 279) and the **outer `loop` recreates the subscription** — which repeats step 2's on-chain read, catching anything missed during the outage.
5. A flat `timeout_deadline` (line 286) gives up with `SettlementError::Timeout(guid)`.

Consequence: **missing the actual log never loses the packet**, because the next trigger (a later DVN event, or a subscription recreation) re-reads authoritative chain state. State-read is strictly better than `eth_getLogs` backfill here — no historical-log replay is needed or wanted.

---

## Three recovery layers (all present in the fork today)

1. **Liveness** — the hint-vs-truth loop above. Survives ws drops within a single process lifetime; auto-recreates on error.
2. **Crash / restart** — interop bundles are persisted in Postgres. `InteropService::new` (`src/transactions/interop.rs:1309`) calls `storage.get_pending_bundles()` on boot and **resumes** in-flight bundles. A Railway redeploy or crash mid-settlement does not strand the bundle.
3. **Ultimate net** — `RefundMonitorService` (`src/interop/refund/monitor.rs`, `DEFAULT_CHECK_INTERVAL_SECS = 60`) polls `get_pending_refunds_ready` and refunds escrows past `escrow_refund_threshold` (`resume_bundle_for_refund` → `process_pending_refunds`). Worst case is refund-with-delay, never loss.

---

## Risk analysis

| Vector | Outcome | Safe? | Notes |
| --- | --- | --- | --- |
| Missed `PayloadVerified` log (ws reconnect gap) | Re-polled on next event or on resubscribe | ✅ | Log is a hint; chain read is truth |
| `broadcast` overflow (>10k unconsumed, `verification.rs:61`) | `Lagged` → treated as stream death → recreate + re-poll | ✅ | Recoverable, but thrashy on a busy chain |
| ws hard error / disconnect | Outer loop recreates subscription + re-polls | ✅ | Explicit error path (line 74–77, 279) |
| Relay process restart mid-settlement | Boot reloads pending bundles and resumes | ✅ | `interop.rs:1309` |
| Verification never happens (DVN stalls, etc.) | Timeout → escrow refund after threshold | ✅ (funds) / ⚠️ (delivery) | User refunded, send not delivered |
| **Silent ws stall (zombie connection)** | No error, no event → no re-poll → rides to timeout → refund | ⚠️ | **Main gap.** Delivery degraded to refund |
| ws provider fully down | Subscriptions AND `is_message_available` reads both fail (shared provider) | ⚠️ | Truth source dies with the socket |
| `wait_verification_timeout` ≥ `escrow_refund_threshold` | Monitor still waiting when refund fires → settle vs already-refunded escrow | ❌ | Config footgun — revert / double-spend risk |
| Late LZ settle message arrives after refund fired | Double release **unless** escrow enforces settle-XOR-refund on-chain | ⚠️ | See atomicity section — funder-loss, user-safe if XOR holds |

---

## Cross-chain refund/settlement atomicity (desync risk)

Refund is where cross-chain desync can happen. The risk is real, bounded, and lands on the **funder, not the user** — provided two invariants hold.

**1. On-chain mutual exclusion in the escrow (load-bearing — lives in the contract, not the relay).**
Each escrow must be one-shot: `settle` (arrives as the LayerZero `lzReceive` message from the destination) and `refund` (time-gated by `refundTimestamp`, `processor.rs:110` / `262`) must be mutually exclusive and idempotent — whichever lands first flips the escrow terminal, the other reverts. The relay does **not** enforce this: `RefundProcessor` only avoids *scheduling* a refund before `refundTimestamp`, then fires a plain `IEscrow::refundCall`. If the escrow contract lacks the settle-XOR-refund guard, both can execute → double release.
→ **Action: confirm the XOR in the escrow contract (`oportet-account`, deployed `0xCd075ceb…`). Not verifiable from the relay repo — it only holds the `IEscrow` ABI + the `refundTimestamp` field.** The e2e `MockEscrow.sol` is a LayerZero `OApp`, so settle arrives via `lzReceive`; the production escrow must gate it against the refund state.

**2. Non-overlapping time windows.**
`refundTimestamp` must be strictly *after* the settlement/verification deadline, so the refund window only opens once settle can no longer land. This is hardening #3 with teeth: `wait_verification_timeout` < time-to-`refundTimestamp`. If the windows overlap, the LZ settle message can arrive on the source chain *after* the refund already fired — the desync.

**Who bears it.** With the XOR held, only one of {settle, refund} wins, so the **user is never double-charged or short-changed**. But the destination leg is funded by our sponsored funder and may already have delivered value on the destination chain. If the source then refunds to the user (because `refundTimestamp` passed before the LZ round-trip released the source escrow to the funder), the funder is out the destination value → **funder-solvency risk, user-safe.**

**"Admin alignment across chains."** Settle authority is a cross-chain LZ message; refund authority is a local time-gate — two independent broadcasters acting on one escrow. Alignment = (a) escrow XOR on-chain + (b) refund window strictly after the settle window. Both → no double-release; residual is only funder-side loss on a genuinely-late-verified packet.

**Mitigations beyond the two invariants:**
- Set `refundTimestamp` generous vs. worst-case DVN verification latency (quorum can take minutes). Hardening #1 + #2 shrink the funder-loss window by not timing out on packets that *were* verified.
- **Canary alert:** any bundle that both settled and refunded should be impossible under XOR — alert on it as a contract-bug signal.
- Future option: escrow-first ordering (destination doesn't release until source-lock is confirmed). Bigger design change — noted, not planned.

---

## Hardening recommendations (ranked)

### 1. Periodic poll tick — closes the silent-stall gap (highest value)

`monitor_packet` only re-reads chain state on a resubscribe or a received event, and a resubscribe only fires on an **explicit** stream error. A zombie websocket — TCP alive, node quietly stops pushing, no error surfaced — triggers neither, so the packet rides to `wait_verification_timeout` and gets refunded instead of delivered.

Add an interval branch to the inner `select!` that re-checks `is_message_available` independent of events:

```rust
let mut poll = tokio::time::interval(Duration::from_secs(15));
// inside the inner loop's select!:
_ = poll.tick() => {
    if self.inner.chain_configs.is_message_available(&packet).await? {
        return Ok(packet.guid);
    }
}
```

Cheap, and it converts silent-stall from timeout into delivery. Also enable **ws keepalive/ping + auto-reconnect** on the alloy `WsConnect` so dead sockets surface as errors and drive the existing resubscribe path.

### 2. Separate http read provider — survive a total ws outage

Today both `subscribe_logs` (line 336) and `is_message_available` read through the same `dst_config.provider`. If that ws is down, the **truth source dies with the socket**.

Give each chain an **http provider for reads** plus ws for subscriptions. Combined with the poll tick, the settler falls back to pure polling and stays correct through a full ws outage. This also removes the single-endpoint SPOF — add a fallback RPC while at it.

### 3. Enforce `wait_verification_timeout` < `escrow_refund_threshold` (config discipline)

`verification.rs:184` carries a live `todo(joshiedo)`: the deadline is a flat `timeout_seconds`, not `refundTimestamp − N`. If config sets the verification timeout at or above the escrow refund threshold, the monitor can still be waiting when the refund fires — then the settlement executes against an already-refunded escrow (revert / double-spend). Set the timeout **well below** the refund threshold, with margin, in `relay.yaml`. Cheap and important.

### 4. `broadcast::channel(10000)` overflow (low priority)

A burst over 10k unconsumed events → `Lagged` → treated as death → recreate + re-poll. Recoverable, just thrashy on a busy chain. Revisit only at high throughput.

---

## Config requirements to enable LayerZero interop

- `[interop]` block in `relay.yaml`: `settler = layerzero`, `wait_verification_timeout` (see #3), escrow refund settings. Escrow contract `0xCd075ceb…` is already in our deployed set.
- `[interop.settler.layerzero]`: `endpoint_addresses` — LayerZero V2 EndpointV2 address per chain. **Confirmed: LayerZero V2 is deployed on Rise mainnet** (and Base mainnet).
- Secret `RELAY_SETTLER_SIGNER_KEY` (env or config) — the settler signer that submits settlement txs. **Fund it per chain.**
- **ws:// RPC per chain** — the settler subscribes to `PayloadVerified` logs via `eth_subscribe` (`Subscription<Log>`); the plain `https://` endpoints in `relay.yaml` are insufficient for subscriptions. Per #2, also provision an http endpoint per chain for state reads.

---

## Decision

Parked. No code change now — this surface only activates when we enable LayerZero interop for mainnet cross-chain. When we do:

1. **Confirm the escrow contract enforces settle-XOR-refund** (`oportet-account`, `0xCd075ceb…`) — the load-bearing atomicity guarantee. Nothing else on this list matters if this is missing.
2. Apply hardening #1 (poll tick + ws keepalive) and #2 (split http/ws provider) as a fork patch on `oportet-relay`.
3. Enforce #3 via `relay.yaml` (`wait_verification_timeout` well below the refund window); set `refundTimestamp` generous vs. worst-case DVN latency.
4. Fund `RELAY_SETTLER_SIGNER_KEY` per chain; wire ws + http RPC per chain; set `endpoint_addresses`.
5. Add the settled-and-refunded canary alert.

Until then, keep cross-chain sends off the LayerZero path (same-chain sponsored sends do not depend on any of this).
