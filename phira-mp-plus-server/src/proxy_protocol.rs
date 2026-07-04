//! PROXY protocol support: trusted-forwarded-for middleware.
//!
//! For the PROXY protocol listener port we trust `X-Forwarded-For` headers
//! set by a reverse proxy (HAProxy, Nginx) and inject the real client IP
//! into request extensions via [`RealClientIp`].  Direct ports skip this.
//!
//! # Architecture
//! - **Direct port** (default 12346 / `--port`): No middleware — the TCP
//!   peer address is authoritative.
//! - **PROXY port** (default 0 / `--proxy-port`, e.g. 12344): The
//!   [`TrustForwardedForLayer`] is applied so that `X-Forwarded-For` headers
//!   from the reverse proxy are trusted.

use std::net::IpAddr;

type Request<B> = axum::http::Request<B>;
type Response<B> = axum::http::Response<B>;

/// Extension key injected into requests that arrived via PROXY protocol.
///
/// Read this from `req.extensions().get::<RealClientIp>()` in handlers
/// that need the original client IP behind a reverse proxy.
#[derive(Debug, Clone, Copy)]
pub struct RealClientIp(pub IpAddr);

/// Tower layer that trusts `X-Forwarded-For` headers and injects
/// [`RealClientIp`] into request extensions.
///
/// Only apply this layer on the PROXY protocol listener port.
#[derive(Clone)]
pub struct TrustForwardedForLayer;

impl<S> tower::Layer<S> for TrustForwardedForLayer {
    type Service = TrustForwardedFor<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TrustForwardedFor(inner)
    }
}

/// Tower service wrapping [`TrustForwardedForLayer`].
#[derive(Clone)]
pub struct TrustForwardedFor<S>(S);

impl<S, ReqBody, ResBody> tower::Service<Request<ReqBody>> for TrustForwardedFor<S>
where
    S: tower::Service<Request<ReqBody>, Response = Response<ResBody>>,
    S::Future: Send + 'static,
    ReqBody: Send + 'static,
    ResBody: Default + Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        tower::Service::poll_ready(&mut self.0, cx)
    }

    fn call(&mut self, mut req: Request<ReqBody>) -> Self::Future {
        // Extract the first IP from X-Forwarded-For header.
        if let Some(value) = req.headers().get("x-forwarded-for") {
            if let Ok(header_str) = value.to_str() {
                if let Some(ip_str) = header_str.split(',').next().map(|s| s.trim()) {
                    if let Ok(ip) = ip_str.parse::<IpAddr>() {
                        req.extensions_mut().insert(RealClientIp(ip));
                    }
                }
            }
        }

        let fut = self.0.call(req);
        Box::pin(async move { fut.await })
    }
}
