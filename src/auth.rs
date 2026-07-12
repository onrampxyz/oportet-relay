//! Better Auth JWT verification for user-tied gas-sponsorship quota.
//!
//! The app's Better Auth `jwt()` plugin issues EdDSA (Ed25519) JWTs whose `sub`
//! is the user id, and serves the public keys as a JWKS at
//! `${authUrl}/api/auth/jwks`. This module verifies a bearer token locally
//! against that JWKS and exposes the verified `sub` to the RPC layer via an http
//! [`Extensions`](http::Extensions) insert, so the sponsorship evaluator can key
//! quota by user rather than by address.
//!
//! Verification is deliberately hand-rolled on `ed25519-dalek` (already in the
//! tree) rather than a JOSE crate: an EdDSA JWT is three base64url segments and
//! the JWKS key is a raw 32-byte Ed25519 public key, so the whole path is a
//! decode + one signature check. Every failure is fail-closed (returns `None`);
//! an unauthenticated request simply carries no `sub` and falls back to
//! address-mode quota.

use std::{
    collections::HashMap,
    task::{Context, Poll},
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use futures::{FutureExt, future::BoxFuture};
use http::header;
use jsonrpsee::{
    core::BoxError,
    server::{HttpBody, HttpRequest, HttpResponse},
};
use serde::Deserialize;
use tokio::sync::RwLock;
use tower::{Layer, Service};
use tracing::{debug, warn};

/// A verified Better Auth subject (`sub` claim = user id), inserted into the
/// request extensions by [`JwtAuthService`] and read by the RPC layer.
#[derive(Clone, Debug)]
pub struct VerifiedSub(pub String);

#[derive(Deserialize)]
struct JwtHeader {
    alg: String,
    kid: String,
}

#[derive(Deserialize)]
struct JwtPayload {
    sub: String,
    exp: i64,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    crv: String,
    /// base64url-encoded raw 32-byte Ed25519 public key.
    x: String,
    kid: String,
}

/// Fetches and caches the app's JWKS, and verifies EdDSA bearer tokens against
/// it. Cheap to clone (shared `Arc` inner via the `RwLock`); one instance is
/// shared across all requests.
#[derive(Clone, Debug)]
pub struct JwksCache {
    client: reqwest::Client,
    jwks_url: String,
    keys: std::sync::Arc<RwLock<HashMap<String, VerifyingKey>>>,
}

impl JwksCache {
    /// Create a cache that fetches keys from `jwks_url`
    /// (e.g. `https://onramp.xyz/api/auth/jwks`).
    pub fn new(jwks_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            jwks_url,
            keys: std::sync::Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Verify a bearer token, returning the `sub` claim on success. Fail-closed:
    /// any malformed segment, unknown key, bad signature, or expired token
    /// yields `None`.
    pub async fn verify(&self, token: &str) -> Option<String> {
        let mut parts = token.split('.');
        let header_b64 = parts.next()?;
        let payload_b64 = parts.next()?;
        let sig_b64 = parts.next()?;
        if parts.next().is_some() {
            return None;
        }

        let header: JwtHeader =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header_b64).ok()?).ok()?;
        if header.alg != "EdDSA" {
            debug!(alg = %header.alg, "jwt: unsupported alg");
            return None;
        }

        let key = self.key_for_kid(&header.kid).await?;

        let sig = Signature::from_slice(&URL_SAFE_NO_PAD.decode(sig_b64).ok()?).ok()?;
        // The JWS signing input is the ASCII `header.payload` (pre-decode bytes).
        let signing_input = format!("{header_b64}.{payload_b64}");
        key.verify_strict(signing_input.as_bytes(), &sig).ok()?;

        let payload: JwtPayload =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload_b64).ok()?).ok()?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
        if payload.exp <= now {
            debug!("jwt: expired");
            return None;
        }

        Some(payload.sub)
    }

    /// Look up a key by `kid`, refreshing the JWKS once on a miss (handles key
    /// rotation without a restart).
    async fn key_for_kid(&self, kid: &str) -> Option<VerifyingKey> {
        if let Some(key) = self.keys.read().await.get(kid).copied() {
            return Some(key);
        }
        // Unknown kid: the app may have rotated its signing key. Refresh once.
        if let Err(err) = self.refresh().await {
            warn!(%err, "jwt: JWKS refresh failed");
            return None;
        }
        self.keys.read().await.get(kid).copied()
    }

    async fn refresh(&self) -> eyre::Result<()> {
        let body = self.client.get(&self.jwks_url).send().await?.bytes().await?;
        let jwks: Jwks = serde_json::from_slice(&body)?;

        let mut map = HashMap::new();
        for jwk in jwks.keys {
            if jwk.kty != "OKP" || jwk.crv != "Ed25519" {
                continue;
            }
            let Ok(bytes) = URL_SAFE_NO_PAD.decode(&jwk.x) else { continue };
            let Ok(bytes): Result<[u8; 32], _> = bytes.try_into() else { continue };
            let Ok(key) = VerifyingKey::from_bytes(&bytes) else { continue };
            map.insert(jwk.kid, key);
        }
        *self.keys.write().await = map;
        Ok(())
    }
}

fn bearer_token(headers: &http::HeaderMap) -> Option<&str> {
    headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ")
}

/// Tower layer that verifies an optional `Authorization: Bearer` JWT and inserts
/// the resulting [`VerifiedSub`] into the request extensions. When constructed
/// without a cache (no `auth` config) it is a pass-through, so address-mode /
/// `sponsor_all` deployments need no JWKS endpoint.
#[derive(Clone, Debug)]
pub struct JwtAuthLayer {
    cache: Option<JwksCache>,
}

impl JwtAuthLayer {
    /// Build the layer. `Some(cache)` enables verification; `None` makes it a
    /// pass-through.
    pub fn new(cache: Option<JwksCache>) -> Self {
        Self { cache }
    }
}

impl<S> Layer<S> for JwtAuthLayer {
    type Service = JwtAuthService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        JwtAuthService { inner, cache: self.cache.clone() }
    }
}

/// The service produced by [`JwtAuthLayer`]; verifies the bearer JWT (if a cache
/// is configured) before delegating to `inner`.
#[derive(Clone, Debug)]
pub struct JwtAuthService<S> {
    inner: S,
    cache: Option<JwksCache>,
}

impl<S, B> Service<HttpRequest<B>> for JwtAuthService<S>
where
    S: Service<HttpRequest<B>, Response = HttpResponse<HttpBody>> + Clone + Send + 'static,
    S::Error: Into<BoxError>,
    S::Future: Send + 'static,
    B: Send + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, mut request: HttpRequest<B>) -> Self::Future {
        let cache = self.cache.clone();
        // Call the ready clone; keep `self.inner` as the fresh clone so we never
        // call a service that wasn't polled ready.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);

        async move {
            if let Some(cache) = cache
                && let Some(token) = bearer_token(request.headers())
                && let Some(sub) = cache.verify(token).await
            {
                request.extensions_mut().insert(VerifiedSub(sub));
            }
            inner.call(request).await.map_err(Into::into)
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn make_jwt(sk: &SigningKey, kid: &str, sub: &str, exp: i64) -> String {
        let header = serde_json::json!({ "alg": "EdDSA", "kid": kid });
        let payload = serde_json::json!({ "sub": sub, "exp": exp });
        let h = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let p = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let signing_input = format!("{h}.{p}");
        let sig = sk.sign(signing_input.as_bytes());
        let s = URL_SAFE_NO_PAD.encode(sig.to_bytes());
        format!("{h}.{p}.{s}")
    }

    async fn preloaded_cache(kid: &str, key: VerifyingKey) -> JwksCache {
        // Port 1 = immediate connection refusal, so an unknown-kid refresh
        // fails fast instead of hanging the test.
        let cache = JwksCache::new("http://127.0.0.1:1/jwks".into());
        cache.keys.write().await.insert(kid.to_string(), key);
        cache
    }

    #[tokio::test]
    async fn verifies_valid_token() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let cache = preloaded_cache("kid-1", sk.verifying_key()).await;
        let token = make_jwt(&sk, "kid-1", "user-123", 32_503_680_000);
        assert_eq!(cache.verify(&token).await.as_deref(), Some("user-123"));
    }

    #[tokio::test]
    async fn rejects_tampered_signature() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let cache = preloaded_cache("kid-1", sk.verifying_key()).await;
        let mut token = make_jwt(&sk, "kid-1", "user-123", 32_503_680_000);
        let last = token.pop().unwrap();
        token.push(if last == 'A' { 'B' } else { 'A' });
        assert!(cache.verify(&token).await.is_none());
    }

    #[tokio::test]
    async fn rejects_expired_token() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let cache = preloaded_cache("kid-1", sk.verifying_key()).await;
        let token = make_jwt(&sk, "kid-1", "user-123", 1);
        assert!(cache.verify(&token).await.is_none());
    }

    #[tokio::test]
    async fn rejects_wrong_key() {
        let signer = SigningKey::from_bytes(&[7u8; 32]);
        let other = SigningKey::from_bytes(&[9u8; 32]);
        // Cache holds `other`'s key under kid-1; token is signed by `signer`.
        let cache = preloaded_cache("kid-1", other.verifying_key()).await;
        let token = make_jwt(&signer, "kid-1", "user-123", 32_503_680_000);
        assert!(cache.verify(&token).await.is_none());
    }
}
