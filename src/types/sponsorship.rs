//! Gas-sponsorship policy + usage types.
//!
//! Model A (relay-side, zero-fee): the relay decides whether to sponsor a call,
//! zeroes the user's intent payment when it does, and its funder pays the real
//! on-chain gas. Sponsored spend is recorded post-receipt into `sponsorship_usage`
//! and drives a global circuit breaker + a per-subject rolling quota.

use std::collections::HashMap;

use alloy::primitives::{
    Address, ChainId, FixedBytes, U256,
    map::{HashMap as AHashMap, HashSet},
};
use serde::{Deserialize, Serialize};

/// What the per-user rolling quota counts against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuotaKey {
    /// Count against the on-chain EOA in the signed intent. The address is
    /// always present, so this needs no JWT — safe default.
    #[default]
    Address,
    /// Count against the Better Auth userId (verified JWT `sub`). Caps a single
    /// human across multiple accounts. Fail-closed when no `sub` is present.
    User,
}

/// Base gas-sponsorship policy. Per-chain overrides live in
/// [`ChainSponsorshipConfig`] and are merged in by [`SponsorshipConfig::resolve`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SponsorshipConfig {
    /// Sponsor every call unconditionally, bypassing every guard, breaker, and
    /// quota. Dev/testnet escape hatch only — never enable on a mainnet relay.
    #[serde(default)]
    pub sponsor_all: bool,
    /// Chain guard: chain ids the relay is allowed to sponsor on (e.g. Base 8453,
    /// Rise 4153, Polygon 137). A call on any chain NOT in this set is never
    /// sponsored. Empty = none (fail-closed).
    #[serde(default)]
    pub sponsored_chains: std::collections::HashSet<ChainId>,
    /// Target guard: contracts our sponsorship is allowed to touch. A sponsored
    /// intent must have EVERY call target either a listed contract or a listed
    /// (contract, selector) pair; anything else is denied (fail-closed). This is
    /// a necessary gate, not a quota exemption — sponsored calls still consume
    /// the per-user quota and count toward the breaker.
    #[serde(default)]
    pub whitelisted_contracts: HashSet<Address>,
    /// Target guard by method: (contract -> allowed 4-byte selectors) additionally
    /// permitted as sponsored targets.
    #[serde(default)]
    pub whitelisted_methods: AHashMap<Address, HashSet<FixedBytes<4>>>,
    /// Global sponsored-spend ceiling per chain over the rolling window (wei).
    /// Reaching it trips the circuit breaker -> deny all further sponsorship.
    pub circuit_breaker_wei: U256,
    /// Primary limiter: per-subject sponsored-spend ceiling over the rolling
    /// window (wei), keyed by `quota_key` (address or verified JWT `sub`).
    pub per_user_wei: U256,
    /// Rolling window for both the breaker and the per-user quota (hours).
    pub window_hours: u64,
    /// Which identity the per-user quota counts against.
    #[serde(default)]
    pub quota_key: QuotaKey,
    /// Per-subject quota overrides (subject -> wei per window). A subject listed
    /// here uses this ceiling instead of `per_user_wei`; subjects not listed use
    /// `per_user_wei`. Used to cap a specific identity — notably the dev
    /// escape-hatch subject — tighter (or looser) than a normal user.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub quota_overrides: HashMap<String, u128>,
}

impl Default for SponsorshipConfig {
    fn default() -> Self {
        // Fail-closed: empty whitelist + sponsor_all off sponsors nothing until a
        // policy is configured. Ceilings seed from the reverted merchant-side design.
        Self {
            sponsor_all: false,
            sponsored_chains: std::collections::HashSet::default(),
            whitelisted_contracts: HashSet::default(),
            whitelisted_methods: AHashMap::default(),
            circuit_breaker_wei: U256::from(1_000_000_000_000_000_000u128), // 1 ETH / window
            per_user_wei: U256::from(10_000_000_000_000_000u128),           // 0.01 ETH / window
            window_hours: 24,
            quota_key: QuotaKey::Address,
            quota_overrides: HashMap::new(),
        }
    }
}

/// Per-chain overrides. Each `Some` field replaces the base value for that chain;
/// `None` falls back to the base [`SponsorshipConfig`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChainSponsorshipConfig {
    /// Override for [`SponsorshipConfig::sponsor_all`].
    #[serde(default)]
    pub sponsor_all: Option<bool>,
    /// Override for [`SponsorshipConfig::whitelisted_contracts`].
    #[serde(default)]
    pub whitelisted_contracts: Option<HashSet<Address>>,
    /// Override for [`SponsorshipConfig::whitelisted_methods`].
    #[serde(default)]
    pub whitelisted_methods: Option<AHashMap<Address, HashSet<FixedBytes<4>>>>,
    /// Override for [`SponsorshipConfig::circuit_breaker_wei`].
    #[serde(default)]
    pub circuit_breaker_wei: Option<U256>,
    /// Override for [`SponsorshipConfig::per_user_wei`].
    #[serde(default)]
    pub per_user_wei: Option<U256>,
    /// Override for [`SponsorshipConfig::window_hours`].
    #[serde(default)]
    pub window_hours: Option<u64>,
    /// Override for [`SponsorshipConfig::quota_key`].
    #[serde(default)]
    pub quota_key: Option<QuotaKey>,
}

impl SponsorshipConfig {
    /// Resolve the effective policy for a chain: per-chain fields override the
    /// base; unset fields fall back to the base.
    pub fn resolve(
        &self,
        chain_id: ChainId,
        overrides: &HashMap<ChainId, ChainSponsorshipConfig>,
    ) -> SponsorshipConfig {
        let Some(o) = overrides.get(&chain_id) else {
            return self.clone();
        };

        SponsorshipConfig {
            sponsor_all: o.sponsor_all.unwrap_or(self.sponsor_all),
            // Chain guard is a relay-global allowlist, not per-chain overridable.
            sponsored_chains: self.sponsored_chains.clone(),
            whitelisted_contracts: o
                .whitelisted_contracts
                .clone()
                .unwrap_or_else(|| self.whitelisted_contracts.clone()),
            whitelisted_methods: o
                .whitelisted_methods
                .clone()
                .unwrap_or_else(|| self.whitelisted_methods.clone()),
            circuit_breaker_wei: o.circuit_breaker_wei.unwrap_or(self.circuit_breaker_wei),
            per_user_wei: o.per_user_wei.unwrap_or(self.per_user_wei),
            window_hours: o.window_hours.unwrap_or(self.window_hours),
            quota_key: o.quota_key.unwrap_or(self.quota_key),
            // Per-subject overrides are relay-global, not per-chain overridable.
            quota_overrides: self.quota_overrides.clone(),
        }
    }
}

/// One sponsored transaction, recorded post-receipt.
#[derive(Debug, Clone)]
pub struct SponsorshipUsage {
    /// On-chain EOA from the signed intent (always recorded, for audit).
    pub user_address: Address,
    /// The identity the per-user quota counts against (EOA or JWT `sub`).
    pub quota_subject: String,
    /// Chain the sponsored transaction landed on.
    pub chain_id: ChainId,
    /// Transaction hash (unique; guards against double-counting).
    pub tx_hash: String,
    /// Gas units consumed on-chain.
    pub gas_used: U256,
    /// Effective gas price paid (wei).
    pub gas_price: U256,
    /// `gas_used * gas_price`, denormalized for cheap windowed SUM aggregation.
    pub eth_spent: U256,
}
