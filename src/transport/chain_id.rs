//! Layer that answers `eth_chainId` without asking the node.

use alloy::{
    primitives::{ChainId, U64},
    rpc::json_rpc::{RequestPacket, Response, ResponsePacket, ResponsePayload},
    transports::{Transport, TransportError, TransportFut},
};
use futures::FutureExt;
use serde_json::value::to_raw_value;
use std::task::{Context, Poll};
use tower::{Layer, Service};

/// A [`tower::Layer`] that answers `eth_chainId` with the configured chain id.
///
/// Every `Provider::get_chain_id` call is an `eth_chainId` request, and the relay makes several
/// per quote. The endpoint's chain is checked once when the provider is built, so the answer
/// cannot change afterwards.
#[derive(Debug, Clone, Copy)]
pub struct ChainIdLayer {
    chain_id: ChainId,
}

impl ChainIdLayer {
    /// Create a new [`ChainIdLayer`] answering with `chain_id`.
    pub const fn new(chain_id: ChainId) -> Self {
        Self { chain_id }
    }
}

impl<T> Layer<T> for ChainIdLayer {
    type Service = ChainIdService<T>;

    fn layer(&self, inner: T) -> Self::Service {
        ChainIdService { inner, chain_id: self.chain_id }
    }
}

/// A service that answers `eth_chainId` itself and forwards every other request.
#[derive(Debug, Clone)]
pub struct ChainIdService<T> {
    inner: T,
    chain_id: ChainId,
}

impl<T> Service<RequestPacket> for ChainIdService<T>
where
    T: Transport + Clone,
{
    type Response = ResponsePacket;
    type Error = TransportError;
    type Future = TransportFut<'static>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: RequestPacket) -> Self::Future {
        if let Some(single) = req.as_single()
            && single.method() == "eth_chainId"
        {
            let response = to_raw_value(&U64::from(self.chain_id)).map(|payload| {
                ResponsePacket::Single(Response {
                    id: single.id().clone(),
                    payload: ResponsePayload::Success(payload),
                })
            });
            return async move { response.map_err(TransportError::ser_err) }.boxed();
        }

        self.inner.call(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::{
        rpc::client::ClientBuilder,
        transports::mock::{Asserter, MockTransport},
    };

    #[tokio::test]
    async fn answers_eth_chain_id_locally() {
        // The asserter has no queued responses, so any request that reaches it fails.
        let asserter = Asserter::new();
        let client = ClientBuilder::default()
            .layer(ChainIdLayer::new(8453))
            .transport(MockTransport::new(asserter.clone()), true);

        let chain_id: U64 = client.request_noparams("eth_chainId").await.unwrap();
        assert_eq!(chain_id, U64::from(8453));

        // Other methods still go to the node.
        asserter.push_success(&U64::from(7));
        let block: U64 = client.request_noparams("eth_blockNumber").await.unwrap();
        assert_eq!(block, U64::from(7));
    }
}
