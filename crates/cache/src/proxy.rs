//! The Pingora proxy: pick the site, decide whether to use the cache, and forward misses.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use pingora_cache::lock::CacheKeyLockImpl;
use pingora_cache::{CacheKey, CacheMeta, CachePhase, NoCacheReason, RespCacheable};
use pingora_core::upstreams::peer::HttpPeer;
use pingora_core::{Error, ErrorType, Result};
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};
use rudder_core::{encoding_class, CacheDocument, CacheRequest, RequestDecision};

use crate::state::{Outcome, Shared};
use crate::storage::primary_key;

pub struct CacheProxy {
    pub shared: &'static Shared,
    pub lock: &'static CacheKeyLockImpl,
}

#[derive(Default)]
pub struct RequestCtx {
    rules: Option<Arc<CacheDocument>>,
    host: String,
    upstream: String,
    policy: Option<usize>,
    rule: Option<usize>,
    key: String,
    encoding: &'static str,
}

fn request_host(header: &RequestHeader) -> String {
    let host = header
        .headers
        .get(http::header::HOST)
        .and_then(|value| value.to_str().ok())
        .or_else(|| header.uri.host())
        .unwrap_or_default();
    host.split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn header_str(header: &RequestHeader, name: http::header::HeaderName) -> Option<&str> {
    header
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
}

#[async_trait]
impl ProxyHttp for CacheProxy {
    type CTX = RequestCtx;

    fn new_ctx(&self) -> Self::CTX {
        RequestCtx::default()
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        let rules = self.shared.rules();
        let header = session.req_header();
        let host = request_host(header);
        let path = header.uri.path().to_owned();
        let Some(site) = rules.site(&host, &path) else {
            session.respond_error(404).await?;
            return Ok(true);
        };
        ctx.upstream = site.upstream.clone();
        ctx.policy = rules
            .policies
            .iter()
            .position(|policy| policy.id == site.policy);
        if let Some(policy) = ctx.policy.map(|index| &rules.policies[index]) {
            let decision = policy.decide(&CacheRequest {
                method: header.method.as_str(),
                path: &path,
                query: header.uri.query(),
                cookie: header_str(header, http::header::COOKIE),
                authorization: header.headers.contains_key(http::header::AUTHORIZATION),
            });
            if let RequestDecision::Lookup { rule, key } = decision {
                ctx.rule = Some(rule);
                ctx.key = key;
                ctx.encoding = encoding_class(header_str(header, http::header::ACCEPT_ENCODING));
            }
        }
        ctx.host = host;
        ctx.rules = Some(rules);
        Ok(false)
    }

    fn request_cache_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<()> {
        let (Some(rules), Some(policy), Some(_)) = (&ctx.rules, ctx.policy, ctx.rule) else {
            return Ok(());
        };
        let max = rules.policies[policy].max_object_bytes();
        session
            .cache
            .enable(self.shared.store, None, None, Some(self.lock), None);
        session
            .cache
            .set_max_file_size_bytes(usize::try_from(max).unwrap_or(usize::MAX));
        Ok(())
    }

    fn cache_key_callback(&self, _session: &Session, ctx: &mut Self::CTX) -> Result<CacheKey> {
        Ok(CacheKey::new(
            primary_key(&ctx.host, ctx.encoding, &ctx.key),
            "",
        ))
    }

    fn response_cache_filter(
        &self,
        _session: &Session,
        resp: &ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<RespCacheable> {
        let (Some(rules), Some(policy), Some(rule)) = (&ctx.rules, ctx.policy, ctx.rule) else {
            return Ok(RespCacheable::Uncacheable(NoCacheReason::NeverEnabled));
        };
        let rule = &rules.policies[policy].rules[rule];
        let text = |name: http::header::HeaderName| {
            let values: Vec<&str> = resp
                .headers
                .get_all(name)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .collect();
            (!values.is_empty()).then(|| values.join(", "))
        };
        let freshness = rule.freshness(
            resp.status.as_u16(),
            text(http::header::CACHE_CONTROL).as_deref(),
            resp.headers.contains_key(http::header::SET_COOKIE),
            text(http::header::VARY).as_deref(),
        );
        let Some(freshness) = freshness else {
            return Ok(RespCacheable::Uncacheable(NoCacheReason::OriginNotCache));
        };
        let now = SystemTime::now();
        Ok(RespCacheable::Cacheable(CacheMeta::new(
            now + Duration::from_secs(u64::from(freshness.ttl_secs)),
            now,
            freshness.stale_while_revalidate_secs,
            freshness.stale_if_error_secs,
            resp.clone(),
        )))
    }

    fn should_serve_stale(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
        error: Option<&Error>,
    ) -> bool {
        // `None` is asked only inside the object's stale-while-revalidate window.
        error.is_none_or(|error| error.esource() == &pingora_core::ErrorSource::Upstream)
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let address = tokio::net::lookup_host(ctx.upstream.as_str())
            .await
            .ok()
            .and_then(|mut addresses| addresses.next())
            .ok_or_else(|| {
                Error::explain(
                    ErrorType::ConnectNoRoute,
                    format!("cannot resolve upstream {}", ctx.upstream),
                )
            })?;
        Ok(Box::new(HttpPeer::new(address, false, String::new())))
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        if session.cache.enabled() {
            match ctx.encoding {
                "identity" => {
                    upstream.remove_header(&http::header::ACCEPT_ENCODING);
                }
                class => upstream.insert_header(http::header::ACCEPT_ENCODING, class)?,
            }
        }
        Ok(())
    }

    async fn response_filter(
        &self,
        session: &mut Session,
        response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()> {
        response.insert_header("x-cache", outcome(session, ctx).header())?;
        Ok(())
    }

    async fn logging(&self, session: &mut Session, _error: Option<&Error>, ctx: &mut Self::CTX) {
        if ctx.host.is_empty() || session.subrequest_ctx.is_some() {
            return;
        }
        let bytes = session.body_bytes_sent() as u64;
        self.shared.record(&ctx.host, outcome(session, ctx), bytes);
    }
}

/// Requests a rule sent to the cache count as a miss even when the response was not storable.
fn outcome(session: &Session, ctx: &RequestCtx) -> Outcome {
    match session.cache.phase() {
        CachePhase::Hit => Outcome::Hit,
        CachePhase::Miss => Outcome::Miss,
        CachePhase::Stale | CachePhase::StaleUpdating => Outcome::Stale,
        CachePhase::Expired => Outcome::Expired,
        CachePhase::Revalidated => Outcome::Revalidated,
        CachePhase::RevalidatedNoCache(_) => Outcome::Miss,
        CachePhase::Disabled(_) | CachePhase::Bypass if ctx.rule.is_some() => Outcome::Miss,
        CachePhase::Disabled(_)
        | CachePhase::Uninit
        | CachePhase::Bypass
        | CachePhase::CacheKey => Outcome::Bypass,
    }
}
