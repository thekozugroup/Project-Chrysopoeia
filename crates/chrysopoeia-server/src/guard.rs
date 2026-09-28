//! Protection of `/api` against other websites.
//!
//! The server has no login (it trusts the local network), but a page on the
//! internet running in a LAN user's browser must not be able to drive it:
//!
//! - **Cross-site requests**: a request that changes something (any method
//!   but GET/HEAD/OPTIONS) and every WebSocket upgrade must come from a page
//!   on Chrysopoeia's own address. Browsers always send `Origin` on those, so
//!   it is compared with the address the request was sent to (`Host`, or
//!   `X-Forwarded-Host` behind a reverse proxy). When that address has no
//!   port (reverse proxies such as Nginx Proxy Manager pass `Host` without
//!   the port the browser used), only the host names are compared; the
//!   host allowlist below still keeps other websites' names out. Requests
//!   without `Origin` or `Referer` (curl, scripts) are not browser requests
//!   and pass.
//! - **DNS rebinding**: a hostile domain that resolves to the server's LAN
//!   address makes the browser treat the API as that domain's own. So the
//!   `Host` header must be an IP address, `localhost`, a local name
//!   (`tower`, `tower.local`, `nas.lan`, `nas.fritz.box`, a Tailscale name)
//!   or a name listed in `ALLOWED_HOSTS`. `X-Forwarded-Host` is not held to
//!   this list: a page can't set it without a CORS preflight (which is never
//!   granted), and it only serves the origin check above.
//!
//! `--dev-cors` (for `next dev` on another port) turns the origin check off.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{OriginalUri, Request, State};
use axum::http::header::{HOST, ORIGIN, REFERER, UPGRADE};
use axum::http::{HeaderMap, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::config::Config;
use crate::error::ApiError;

/// Suffixes of names that only resolve inside a home network (or a
/// tailnet), which a website can't point at the server.
const LOCAL_SUFFIXES: &[&str] = &[
    ".local",
    ".lan",
    ".home",
    ".home.arpa",
    ".internal",
    ".localdomain",
    ".localhost",
    ".ts.net",
    // Home networks behind an AVM FRITZ!Box router.
    ".fritz.box",
];

/// Advice added to refusals that a reverse proxy can cause.
const PROXY_ADVICE: &str = "If you use a reverse proxy, make it pass the original Host header.";

/// Which requests `/api` accepts.
#[derive(Debug, Clone, Default)]
pub struct RequestGuard {
    /// Extra host names (lowercase), or entries starting with `.` for a
    /// whole domain.
    allowed_hosts: Vec<String>,
    /// `ALLOWED_HOSTS=*`: any host name.
    any_host: bool,
    /// `--dev-cors`: skip the origin check.
    dev_cors: bool,
}

impl RequestGuard {
    /// The guard for a configuration.
    pub fn new(config: &Config) -> Self {
        Self {
            any_host: config.allowed_hosts.iter().any(|h| h == "*"),
            allowed_hosts: config.allowed_hosts.clone(),
            dev_cors: config.dev_cors,
        }
    }

    /// Whether the server answers to this host name (without port).
    pub fn host_allowed(&self, name: &str) -> bool {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        if self.any_host || name.is_empty() {
            return true;
        }
        let bare = name.trim_start_matches('[').trim_end_matches(']');
        if bare.parse::<IpAddr>().is_ok() || name == "localhost" || !name.contains('.') {
            return true;
        }
        if LOCAL_SUFFIXES.iter().any(|s| name.ends_with(s)) {
            return true;
        }
        self.allowed_hosts.iter().any(|allowed| {
            if let Some(domain) = allowed.strip_prefix('.') {
                name == domain || name.ends_with(allowed.as_str())
            } else {
                name == *allowed
            }
        })
    }

    /// Check one request.
    pub fn check(
        &self,
        method: &Method,
        uri_host: Option<&str>,
        headers: &HeaderMap,
    ) -> Result<(), ApiError> {
        let host = header_str(headers, HOST.as_str()).or(uri_host);
        if let Some(host) = host {
            let (name, _) = split_host_port(host);
            if !self.host_allowed(name) {
                return Err(ApiError::forbidden(
                    "host_not_allowed",
                    format!(
                        "Chrysopoeia doesn't answer to the address \"{name}\". If you reach it \
                         through a domain name (for example behind a reverse proxy), add that \
                         name to ALLOWED_HOSTS in the container settings. {PROXY_ADVICE}"
                    ),
                ));
            }
        }
        if self.dev_cors {
            return Ok(());
        }
        let changes_something = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
        let websocket = header_str(headers, UPGRADE.as_str())
            .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
        if !changes_something && !websocket {
            return Ok(());
        }
        let source =
            header_str(headers, ORIGIN.as_str()).or_else(|| header_str(headers, REFERER.as_str()));
        let Some(source) = source else {
            return Ok(());
        };
        let targets = [
            host,
            header_str(headers, "x-forwarded-host")
                .and_then(|v| v.split(',').next())
                .map(str::trim),
        ];
        if targets
            .into_iter()
            .flatten()
            .any(|target| same_origin(source, target))
        {
            return Ok(());
        }
        Err(ApiError::forbidden(
            "forbidden_origin",
            format!(
                "This request came from another website, so Chrysopoeia refused it. Open \
                 Chrysopoeia at its own address to make changes. {PROXY_ADVICE}"
            ),
        ))
    }
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// Split `host[:port]` (IPv6 in brackets) into the name and the port.
pub fn split_host_port(value: &str) -> (&str, Option<&str>) {
    if let Some(rest) = value.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((ip, tail)) => (ip, tail.strip_prefix(':').filter(|p| !p.is_empty())),
            None => (value, None),
        };
    }
    match value.rsplit_once(':') {
        // More than one colon without brackets: a bare IPv6 address.
        Some((name, _)) if name.contains(':') => (value, None),
        Some((name, port)) => (name, Some(port).filter(|p| !p.is_empty())),
        None => (value, None),
    }
}

/// Whether `source` (an `Origin` or `Referer` URL) points at `target` (a
/// `Host` value). A port left out of `source` means the scheme's default
/// port. A `target` without a port matches any port: reverse proxies (Nginx
/// Proxy Manager, Traefik, Caddy) often pass the host name alone, even when
/// the browser used a port such as 8443.
fn same_origin(source: &str, target: &str) -> bool {
    let Some((scheme, rest)) = source.split_once("://") else {
        // `Origin: null` (sandboxed pages, file://) and anything unparsable.
        return false;
    };
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => "80",
        "https" => "443",
        _ => return false,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Drop any user info (`user@host`).
    let authority = authority.rsplit('@').next().unwrap_or_default();
    let (source_name, source_port) = split_host_port(authority);
    let (target_name, target_port) = split_host_port(target);
    if !source_name.eq_ignore_ascii_case(target_name) {
        return false;
    }
    match target_port {
        Some(port) => source_port.unwrap_or(default_port) == port,
        None => true,
    }
}

/// Middleware applying a [`RequestGuard`] to every request it wraps.
pub async fn middleware(
    State(guard): State<Arc<RequestGuard>>,
    req: Request,
    next: Next,
) -> Response {
    let uri_host = req.uri().authority().map(|a| a.as_str().to_string());
    match guard.check(req.method(), uri_host.as_deref(), req.headers()) {
        Ok(()) => next.run(req).await,
        Err(e) => {
            // Inside `/api` the URI is relative to it; log the full path.
            let path = req
                .extensions()
                .get::<OriginalUri>()
                .map_or_else(|| req.uri().path().to_string(), |u| u.path().to_string());
            tracing::warn!(
                method = %req.method(),
                %path,
                "refused a request: {}",
                e.message
            );
            e.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn guard(allowed: &[&str]) -> RequestGuard {
        RequestGuard::new(&Config {
            allowed_hosts: allowed.iter().map(|s| (*s).to_string()).collect(),
            ..Config::default()
        })
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn local_hosts_are_allowed_and_others_need_listing() {
        let g = guard(&["media.example.com", ".mydomain.org"]);
        for ok in [
            "192.168.1.10",
            "::1",
            "[fe80::1]",
            "localhost",
            "tower",
            "Tower.Local",
            "nas.lan",
            "nas.home.arpa",
            "tower.tail1234.ts.net",
            "nas.fritz.box",
            "media.example.com",
            "chrysopoeia.mydomain.org",
            "mydomain.org",
        ] {
            assert!(g.host_allowed(ok), "{ok}");
        }
        for bad in ["attacker.example", "example.com.evil.net", "local.evil.com"] {
            assert!(!g.host_allowed(bad), "{bad}");
        }
        assert!(guard(&["*"]).host_allowed("anything.example"));
    }

    #[test]
    fn host_and_port_split() {
        assert_eq!(split_host_port("tower:8080"), ("tower", Some("8080")));
        assert_eq!(split_host_port("tower"), ("tower", None));
        assert_eq!(split_host_port("[::1]:8080"), ("::1", Some("8080")));
        assert_eq!(split_host_port("::1"), ("::1", None));
    }

    #[test]
    fn origins_compare_by_name_and_port() {
        assert!(same_origin("http://tower:8080", "tower:8080"));
        assert!(same_origin("http://TOWER:8080", "tower:8080"));
        assert!(same_origin(
            "https://media.example.com",
            "media.example.com"
        ));
        assert!(same_origin(
            "https://media.example.com",
            "media.example.com:443"
        ));
        assert!(same_origin("http://tower:8080/queue?x=1", "tower:8080"));
        assert!(same_origin("http://[::1]:8080", "[::1]:8080"));
        assert!(!same_origin("http://tower:3000", "tower:8080"));
        assert!(!same_origin("http://evil.example", "tower:8080"));
        // A proxy that passes the host name without the port.
        assert!(same_origin(
            "https://media.example.com:8443",
            "media.example.com"
        ));
        assert!(same_origin("http://tower:8080", "tower"));
        assert!(!same_origin(
            "https://evil.example:8443",
            "media.example.com"
        ));
        assert!(!same_origin("null", "tower:8080"));
        assert!(!same_origin("file:///x", "tower:8080"));
    }

    #[test]
    fn changes_and_websockets_need_a_matching_origin() {
        let g = guard(&[]);
        let post = Method::POST;
        let get = Method::GET;
        // Same origin, directly or behind a proxy.
        let h = headers(&[("host", "tower:8080"), ("origin", "http://tower:8080")]);
        assert!(g.check(&post, None, &h).is_ok());
        let h = headers(&[
            ("host", "192.168.1.5:8080"),
            ("x-forwarded-host", "tower.lan"),
            ("origin", "https://tower.lan"),
        ]);
        assert!(g.check(&post, None, &h).is_ok());
        // Another site.
        let h = headers(&[("host", "tower:8080"), ("origin", "http://evil.example")]);
        assert_eq!(
            g.check(&post, None, &h).unwrap_err().code,
            "forbidden_origin"
        );
        let h = headers(&[("host", "tower:8080"), ("referer", "http://evil.example/x")]);
        assert!(g.check(&post, None, &h).is_err());
        // A cross-site WebSocket.
        let h = headers(&[
            ("host", "tower:8080"),
            ("origin", "http://evil.example"),
            ("upgrade", "websocket"),
        ]);
        assert!(g.check(&get, None, &h).is_err());
        // Reads and non-browser clients pass.
        let h = headers(&[("host", "tower:8080"), ("origin", "http://evil.example")]);
        assert!(g.check(&get, None, &h).is_ok());
        let h = headers(&[("host", "tower:8080")]);
        assert!(g.check(&post, None, &h).is_ok());
        // Nginx Proxy Manager: Host without the port the browser used.
        let g2 = guard(&["name"]);
        let h = headers(&[("host", "name"), ("origin", "https://name:8443")]);
        assert!(g2.check(&post, None, &h).is_ok());
        let h = headers(&[("host", "name"), ("origin", "https://other:8443")]);
        assert_eq!(
            g2.check(&post, None, &h).unwrap_err().code,
            "forbidden_origin"
        );
        let e = g
            .check(
                &post,
                None,
                &headers(&[("host", "tower:8080"), ("origin", "http://evil.example")]),
            )
            .unwrap_err();
        assert!(e.message.ends_with(PROXY_ADVICE), "{}", e.message);
        // DNS rebinding: a foreign host name is refused whatever the origin.
        let h = headers(&[
            ("host", "attacker.example:8080"),
            ("origin", "http://attacker.example:8080"),
        ]);
        let e = g.check(&get, None, &h).unwrap_err();
        assert_eq!(e.code, "host_not_allowed");
        assert!(e.message.ends_with(PROXY_ADVICE), "{}", e.message);
    }

    #[test]
    fn dev_cors_skips_the_origin_check_only() {
        let g = RequestGuard::new(&Config {
            dev_cors: true,
            ..Config::default()
        });
        let h = headers(&[
            ("host", "localhost:8080"),
            ("origin", "http://localhost:3000"),
        ]);
        assert!(g.check(&Method::POST, None, &h).is_ok());
        let h = headers(&[("host", "attacker.example")]);
        assert!(g.check(&Method::GET, None, &h).is_err());
    }
}
