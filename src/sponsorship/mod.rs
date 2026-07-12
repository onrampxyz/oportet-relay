//! Relay-side gas-sponsorship evaluator (Model A, zero-fee).
//!
//! Decides whether a set of calls should be sponsored (whitelist -> circuit
//! breaker -> per-subject rolling quota) and records sponsored spend after a
//! successful send. The relay's own funder pays the on-chain gas; the user's
//! intent payment is zeroed by the caller when this returns `true`.

use std::collections::HashMap;

use alloy::primitives::{Address, ChainId, FixedBytes};
use tracing::{debug, warn};

use crate::{
    error::StorageError,
    storage::{RelayStorage, StorageApi},
    types::{Call, ChainSponsorshipConfig, QuotaKey, SponsorshipConfig},
};

/// Evaluates gas-sponsorship policy against the usage ledger.
#[derive(Debug, Clone)]
pub struct SponsorshipEvaluator {
    base: SponsorshipConfig,
    per_chain: HashMap<ChainId, ChainSponsorshipConfig>,
    storage: RelayStorage,
}

impl SponsorshipEvaluator {
    /// Creates an evaluator from the base policy, per-chain overrides, and the
    /// usage-ledger storage.
    pub fn new(
        base: SponsorshipConfig,
        per_chain: HashMap<ChainId, ChainSponsorshipConfig>,
        storage: RelayStorage,
    ) -> Self {
        Self { base, per_chain, storage }
    }

    /// Effective policy for `chain_id` (base merged with per-chain overrides).
    fn config_for(&self, chain_id: ChainId) -> SponsorshipConfig {
        self.base.resolve(chain_id, &self.per_chain)
    }

    /// The identity the per-user quota counts against, per policy. `None` means
    /// deny: `quota_key = user` but no verified userId was supplied.
    fn quota_subject(
        cfg: &SponsorshipConfig,
        eoa: Address,
        user_id: Option<&str>,
    ) -> Option<String> {
        match cfg.quota_key {
            QuotaKey::Address => Some(eoa.to_string()),
            QuotaKey::User => user_id.map(str::to_owned),
        }
    }

    /// The subject a sponsored tx on `chain_id` should be recorded against,
    /// resolved through that chain's policy. Used at send time so the usage
    /// ledger keys the SAME subject the decision counted (address or verified
    /// user). `None` under `quota_key = user` with no verified user — in which
    /// case the tx would not have been sponsored, so nothing is recorded.
    pub fn resolve_quota_subject(
        &self,
        eoa: Address,
        user_id: Option<&str>,
        chain_id: ChainId,
    ) -> Option<String> {
        Self::quota_subject(&self.config_for(chain_id), eoa, user_id)
    }

    /// Whether every call targets a whitelisted contract or method. Empty call
    /// lists are never whitelisted (fail-closed).
    fn is_whitelisted(cfg: &SponsorshipConfig, calls: &[Call]) -> bool {
        !calls.is_empty()
            && calls.iter().all(|call| {
                let by_contract = cfg.whitelisted_contracts.contains(&call.to);
                let by_method = cfg
                    .whitelisted_methods
                    .get(&call.to)
                    .is_some_and(|sels| selector(&call.data).is_some_and(|s| sels.contains(&s)));
                by_contract || by_method
            })
    }

    /// Evaluate whether `calls` should be gas-sponsored on `chain_id`.
    ///
    /// `user_id` is the verified Better Auth `sub` (from the gateway/JWT), used
    /// only when the policy's `quota_key = user`.
    pub async fn is_sponsored(
        &self,
        eoa: Address,
        user_id: Option<&str>,
        calls: &[Call],
        chain_id: ChainId,
    ) -> Result<bool, StorageError> {
        let cfg = self.config_for(chain_id);

        if cfg.sponsor_all {
            return Ok(true);
        }

        // Whitelisted flows are always sponsored, quota-exempt.
        if Self::is_whitelisted(&cfg, calls) {
            debug!(%eoa, chain_id, "sponsorship: whitelisted call");
            return Ok(true);
        }

        // Global circuit breaker: once the chain's rolling sponsored spend hits
        // the ceiling, deny all non-whitelisted sponsorship.
        let global =
            self.storage.global_sponsored_wei_in_window(chain_id, cfg.window_hours).await?;
        if global >= cfg.circuit_breaker_wei {
            warn!(%eoa, chain_id, "sponsorship denied: circuit breaker active");
            return Ok(false);
        }

        // Per-subject rolling quota.
        let Some(subject) = Self::quota_subject(&cfg, eoa, user_id) else {
            warn!(%eoa, chain_id, "sponsorship denied: quota_key=user but no verified userId");
            return Ok(false);
        };
        let spent =
            self.storage.sponsored_wei_in_window(&subject, chain_id, cfg.window_hours).await?;
        if spent >= cfg.per_user_wei {
            debug!(%eoa, chain_id, "sponsorship denied: per-user quota exhausted");
            return Ok(false);
        }

        Ok(true)
    }
}

/// First 4 bytes of calldata as a selector, if present.
fn selector(data: &[u8]) -> Option<FixedBytes<4>> {
    (data.len() >= 4).then(|| FixedBytes::<4>::from_slice(&data[..4]))
}
