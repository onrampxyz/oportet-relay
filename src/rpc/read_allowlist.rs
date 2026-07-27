//! The (chain, contract) pairs `wallet_ethCall` will read from.
//!
//! # Why an allowlist rather than a passthrough
//!
//! The relay holds an authenticated connection to every chain we support. An
//! unrestricted `eth_call` on it is an open, sponsored archive node: any
//! authenticated caller could read any contract, at our expense and under our
//! egress identity, and use us to reach hosts they cannot reach themselves.
//! `eth_call` is also not free of side effects in the way it sounds — it burns
//! upstream provider quota, and a caller who can pick `to` can pick a contract
//! whose view function loops until the provider times out.
//!
//! So the method reads a fixed, reviewed set of contracts and nothing else.
//! Adding a contract is a code change with a diff, which is the point.
//!
//! # Why these entries
//!
//! Every one of them is a read the app already performs today, directly from
//! the device against a public RPC. Routing them here removes the device's
//! dependency on a third-party endpoint's rate limits and uptime without
//! widening what can be read.

use alloy::primitives::{Address, Bytes, ChainId, address};
use serde::{Deserialize, Serialize};

/// Polygon mainnet, where every Polymarket contract we read lives.
pub const POLYGON: ChainId = 137;

/// The reads `wallet_ethCall` will serve, as `(chain, contract)`.
///
/// KEEP THIS SORTED BY CHAIN THEN ADDRESS — it is scanned linearly and read by
/// humans far more often than by the CPU.
pub const READ_ALLOWLIST: &[(ChainId, Address)] = &[
    // Deposit-wallet factory — the address derivation the app corroborates
    // across two providers before it trusts a wallet address.
    (POLYGON, address!("00000000000Fb5C9ADea0298D729A0CB3823Cc07")),
    // USDC.e — balance and allowance for the deposit wallet's collateral.
    (POLYGON, address!("2791Bca1f2de4661ED88A30C99A7a9449Aa84174")),
    // Collateral offramp — pUSD to USDC.e.
    (POLYGON, address!("2957922Eb93258b93368531d39fAcCA3B4dC5854")),
    // Conditional tokens — `isApprovedForAll` for the trading operators.
    (POLYGON, address!("4D97DCd97eC945f40cF65F87097ACe5EA0476045")),
    // Collateral onramp — USDC.e to pUSD.
    (POLYGON, address!("93070a847efEf7F70739046A929D47a521F5B8ee")),
    // pUSD — the collateral the exchange actually counts.
    (POLYGON, address!("C011a7E12a19f7B1f670d46F03B03f3342E82DFB")),
];

/// Parameters for `wallet_ethCall`.
///
/// DELIBERATELY NOT A `TransactionRequest`. A caller gets a chain, a contract
/// and calldata, and nothing else — no `from`, no `value`, no `gas`, no block
/// tag, no state overrides. Each of those is a way to make the relay do work
/// or say something on a caller's behalf that a read has no business doing,
/// and none of the reads this serves need any of them. Widening this struct is
/// the change to think hardest about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EthCallParameters {
    /// The chain to read on.
    pub chain_id: ChainId,
    /// The contract to read. Must be allowlisted for `chain_id`.
    pub to: Address,
    /// ABI-encoded calldata for the view function.
    pub data: Bytes,
}

/// Whether `wallet_ethCall` may read `to` on `chain`.
///
/// Case-insensitive by construction: [`Address`] compares as bytes, so a
/// caller's checksum casing cannot change the answer. That matters — an
/// allowlist that a caller can miss by sending lowercase is not an allowlist.
pub fn is_read_allowed(chain: ChainId, to: Address) -> bool {
    READ_ALLOWLIST.iter().any(|&(c, a)| c == chain && a == to)
}

#[cfg(test)]
mod tests {
    use super::*;

    const USDC_E: Address = address!("2791Bca1f2de4661ED88A30C99A7a9449Aa84174");
    const FACTORY: Address = address!("00000000000Fb5C9ADea0298D729A0CB3823Cc07");

    #[test]
    fn allows_a_listed_pair() {
        assert!(is_read_allowed(POLYGON, USDC_E));
        assert!(is_read_allowed(POLYGON, FACTORY));
    }

    #[test]
    fn rejects_an_unlisted_contract_on_a_listed_chain() {
        // A real contract, deliberately not ours to read: Polygon's WMATIC.
        assert!(!is_read_allowed(POLYGON, address!("0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270")));
    }

    #[test]
    fn rejects_a_listed_contract_on_the_wrong_chain() {
        // The same address is very often deployed on several chains. The pair
        // is the unit of trust, never the address alone.
        for chain in [1, 8453, 42161, 11155111] {
            assert!(!is_read_allowed(chain, USDC_E), "chain {chain} must not match");
        }
    }

    #[test]
    fn casing_cannot_smuggle_an_address_past_the_check() {
        let lower: Address = "0x2791bca1f2de4661ed88a30c99a7a9449aa84174".parse().unwrap();
        let upper: Address = "0x2791BCA1F2DE4661ED88A30C99A7A9449AA84174".parse().unwrap();
        assert!(is_read_allowed(POLYGON, lower));
        assert!(is_read_allowed(POLYGON, upper));
    }

    #[test]
    fn rejects_the_zero_address() {
        // `to: null` is a contract creation and has no business here; the zero
        // address is the closest a caller can get to it through this API.
        assert!(!is_read_allowed(POLYGON, Address::ZERO));
    }

    #[test]
    fn the_list_has_no_duplicates() {
        // A duplicate is harmless at runtime but always means a bad merge.
        let mut seen = READ_ALLOWLIST.to_vec();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), READ_ALLOWLIST.len(), "duplicate entry in READ_ALLOWLIST");
    }

    #[test]
    fn the_list_is_sorted_as_documented() {
        let mut sorted = READ_ALLOWLIST.to_vec();
        sorted.sort();
        assert_eq!(sorted, READ_ALLOWLIST.to_vec(), "READ_ALLOWLIST is not sorted");
    }
}
