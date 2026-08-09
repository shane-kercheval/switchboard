//! The OAuth validation preflight: the mandatory MCP-authorization-spec checks
//! that `rmcp` omits, run before any credential is sent or browser opened. Both
//! the OAuth-mode client construction and the sign-in flow call this — there is
//! exactly one validation boundary.
//!
//! Each gate maps to a hole in rmcp 1.7.0's auth module:
//!
//! - **Resource identity (RFC 9728 §3.3).** rmcp's `ResourceServerMetadata`
//!   deserializes only `authorization_server(s)` and `scopes_supported` — it
//!   has no `resource` field, so validating that the advertised resource is the
//!   server we asked about is structurally impossible with rmcp as shipped. We
//!   parse the protected-resource document ourselves.
//! - **HTTPS enforcement.** rmcp has two scheme checks and *neither is on our
//!   path*: `is_https_url` guards only the client-metadata-URL (SEP-991) path
//!   (auth.rs:2137), and the token-endpoint HTTPS check at auth.rs:1983 belongs
//!   to the client-credentials flow. Anyone who finds those helpers will assume
//!   the authorization-code flow is covered; it is not.
//! - **PKCE refusal.** rmcp's `validate_server_metadata` warns and proceeds
//!   when `S256` is absent; the spec requires refusal: *"If
//!   `code_challenge_methods_supported` is absent, the authorization server
//!   does not support PKCE and MCP clients MUST refuse to proceed."*
//!
//! The validated metadata must be installed with
//! `AuthorizationManager::set_metadata` so rmcp never re-discovers its own copy
//! — validating one document while rmcp fetches another would be decorative.
//! That holds because we drive registration and authorization manually;
//! `OAuthState::start_authorization` rediscovers unconditionally, which is one
//! of the reasons the sign-in flow does not use it.
//!
//! **Loopback exception.** Every HTTPS gate exempts loopback hosts (127.0.0.0/8,
//! `::1`, `localhost`): that traffic never leaves the machine, and it is what
//! lets dev servers and hermetic tests run the real flow. Real deployments of
//! RFC 9728/8414 metadata are HTTPS.

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::error::PromptError;
use crate::oauth::canonicalize_resource_url;
use rmcp::transport::auth::AuthorizationMetadata;

/// The preflight's product: metadata every gate has passed, ready for
/// `set_metadata`, plus the scopes resolved per the spec's priority order.
#[derive(Debug)]
pub(crate) struct PreflightOutcome {
    pub metadata: AuthorizationMetadata,
    /// Scopes to request at registration and authorization, in priority order:
    /// per-provider config override, then the initial 401's `scope` challenge,
    /// then the protected-resource `scopes_supported`. `None` means **omit the
    /// scope parameter entirely** — a generic client has no basis to guess
    /// another product's permissions, and rmcp's own fallback (the
    /// *authorization server's* `scopes_supported`) would over-ask (e.g.
    /// requesting Clerk's `private_metadata` to fetch prompt templates), so
    /// that path never runs.
    pub scopes: Option<Vec<String>>,
}

/// RFC 9728 protected-resource metadata, parsed by us because rmcp discards the
/// `resource` field (see module docs).
#[derive(Debug, Deserialize)]
struct ProtectedResourceMetadata {
    resource: Option<String>,
    #[serde(default)]
    authorization_servers: Vec<String>,
    authorization_server: Option<String>,
    scopes_supported: Option<Vec<String>>,
}

/// What an unauthenticated probe of the MCP endpoint yielded.
#[derive(Debug, Default)]
struct Challenge {
    /// The `resource_metadata` pointer from `WWW-Authenticate`, if present.
    resource_metadata: Option<String>,
    /// The `scope` challenge from `WWW-Authenticate`, if present.
    scope: Option<String>,
}

/// Run every mandatory gate against `canonical_url` (already canonicalized —
/// see `oauth::canonicalize_resource_url`) and resolve the scopes to request.
/// All requests here are unauthenticated; no credential can leak through this
/// path. Errors name the failing gate, with both values on identity mismatches.
pub(crate) async fn preflight(
    http: &reqwest::Client,
    provider: &str,
    canonical_url: &str,
    scopes_override: Option<&[String]>,
) -> Result<PreflightOutcome, PromptError> {
    let provider_url = url::Url::parse(canonical_url).map_err(|e| {
        gate(
            provider,
            format!("invalid provider URL {canonical_url:?}: {e}"),
        )
    })?;
    // RFC 9728 excludes fragments from resource identifiers.
    if provider_url.fragment().is_some() {
        return Err(gate(
            provider,
            format!("provider URL {canonical_url:?} must not contain a fragment"),
        ));
    }
    let provider_is_loopback = is_loopback_host(&provider_url);
    require_safe_url(
        provider,
        provider_is_loopback,
        "provider URL",
        &provider_url,
    )?;

    let challenge = probe_challenge(http, canonical_url).await;

    // The challenge pointer is server-controlled text: it must pass the same
    // safety gate as every other fetched URL, or it degrades to the well-known
    // fallbacks (which are derived from the already-validated provider origin).
    let pointer = challenge.resource_metadata.clone().filter(|pointer| {
        match require_safe_url_str(provider, provider_is_loopback, "resource_metadata pointer", pointer) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "ignoring unsafe WWW-Authenticate resource_metadata pointer; using well-known discovery");
                false
            }
        }
    });
    let prm_candidates: Vec<String> = pointer
        .into_iter()
        .chain(well_known_prm_urls(&provider_url))
        .collect();
    let prm: ProtectedResourceMetadata = fetch_first_json(
        http,
        provider,
        "protected-resource metadata",
        &prm_candidates,
    )
    .await?;

    validate_advertised_resource(provider, canonical_url, prm.resource.as_deref())?;

    // RFC 9728 permits a list of authorization servers; we deliberately use
    // only the first — trying each would multiply discovery traffic for a
    // configuration no known server uses.
    let auth_server = prm
        .authorization_servers
        .first()
        .cloned()
        .or(prm.authorization_server)
        .ok_or_else(|| {
            gate(
                provider,
                "protected-resource metadata names no authorization server".to_owned(),
            )
        })?;
    let auth_server_url = url::Url::parse(&auth_server).map_err(|e| {
        gate(
            provider,
            format!("invalid authorization server URL {auth_server:?}: {e}"),
        )
    })?;
    // The AS identifier is also server-controlled: gate it before deriving and
    // fetching its metadata URLs, and require a clean issuer identifier (RFC
    // 8414 §2: no query or fragment).
    require_safe_url(
        provider,
        provider_is_loopback,
        "authorization server URL",
        &auth_server_url,
    )?;
    if auth_server_url.query().is_some() || auth_server_url.fragment().is_some() {
        return Err(gate(
            provider,
            format!(
                "authorization server URL {auth_server:?} must not contain a query or fragment"
            ),
        ));
    }

    let metadata: AuthorizationMetadata = fetch_first_json(
        http,
        provider,
        "authorization-server metadata",
        &well_known_as_urls(&auth_server_url),
    )
    .await?;
    validate_as_metadata(
        provider,
        provider_is_loopback,
        &auth_server,
        &auth_server_url,
        &metadata,
    )?;

    let scopes = scopes_override
        .map(<[String]>::to_vec)
        .or_else(|| {
            challenge
                .scope
                .as_deref()
                .map(|s| s.split_whitespace().map(str::to_owned).collect())
        })
        .or(prm.scopes_supported)
        .filter(|scopes: &Vec<String>| !scopes.is_empty());

    Ok(PreflightOutcome { metadata, scopes })
}

/// Resource identity (RFC 9728 §3.3), canonical form on BOTH sides — a
/// conformant server may spell the same resource with an uppercase host or an
/// explicit default port, and refusing it would hand the user an error
/// comparing two strings that look identical.
fn validate_advertised_resource(
    provider: &str,
    canonical_url: &str,
    advertised: Option<&str>,
) -> Result<(), PromptError> {
    let Some(advertised) = advertised else {
        return Err(gate(
            provider,
            "protected-resource metadata omits the required `resource` field".to_owned(),
        ));
    };
    let advertised_canonical =
        canonicalize_resource_url(advertised).unwrap_or_else(|_| advertised.to_owned());
    if advertised_canonical != canonical_url {
        return Err(gate(
            provider,
            format!(
                "protected-resource metadata advertises resource {advertised:?} but this provider is configured for {canonical_url:?}"
            ),
        ));
    }
    Ok(())
}

/// The authorization-server identifier form used for **both** discovery
/// candidates and the issuer comparison — a single derivation, so the two
/// cannot drift (candidates trimming a trailing slash the comparison keeps
/// would refuse conformant servers over one invisible character).
///
/// This is a deliberate, narrowly-scoped relaxation of RFC 8414 §3.3's raw
/// string comparison, serving its purpose (the returned issuer must denote the
/// identity actually queried): the equivalences accepted are scheme/host case,
/// an explicit default port, parser-level dot-segment normalization, and a
/// trailing slash — exactly the spellings that still name the same authority
/// the well-known request was sent to.
///
/// Crate-visible: the sign-in flow's registration-reuse check compares stored
/// vs. current issuer through this same derivation, so a spelling-level
/// difference costs nothing there either (a phantom mismatch would merely
/// trigger one harmless re-registration, but reuse exists to avoid exactly
/// that churn).
pub(crate) fn as_identifier(url: &url::Url) -> String {
    format!("{}{}", origin_of(url), url.path().trim_end_matches('/'))
}

/// The gates on fetched authorization-server metadata: issuer binding, endpoint
/// safety, DCR availability, and the PKCE refusal rule.
fn validate_as_metadata(
    provider: &str,
    provider_is_loopback: bool,
    auth_server: &str,
    auth_server_url: &url::Url,
    metadata: &AuthorizationMetadata,
) -> Result<(), PromptError> {
    // RFC 8414 §3.3 issuer binding — the mix-up-attack defense: the metadata
    // must claim exactly the identity we asked about. Both sides go through
    // `as_identifier` (see its docs for the accepted equivalences); raw values
    // in the error.
    let Some(issuer) = metadata.issuer.as_deref() else {
        return Err(gate(
            provider,
            "authorization-server metadata omits the required `issuer` field".to_owned(),
        ));
    };
    let issuer_matches = url::Url::parse(issuer)
        .is_ok_and(|issuer_url| as_identifier(&issuer_url) == as_identifier(auth_server_url));
    if !issuer_matches {
        return Err(gate(
            provider,
            format!(
                "authorization-server metadata claims issuer {issuer:?} but was discovered via {auth_server:?}"
            ),
        ));
    }

    // The same safety gate on the three endpoints where codes and tokens
    // actually travel — not merely on the issuer.
    require_safe_url_str(
        provider,
        provider_is_loopback,
        "authorization_endpoint",
        &metadata.authorization_endpoint,
    )?;
    require_safe_url_str(
        provider,
        provider_is_loopback,
        "token_endpoint",
        &metadata.token_endpoint,
    )?;
    let registration_endpoint = metadata.registration_endpoint.as_deref().ok_or_else(|| {
        gate(
            provider,
            "authorization server offers no registration_endpoint — dynamic client registration is required (pre-registered client ids are not supported)"
                .to_owned(),
        )
    })?;
    require_safe_url_str(
        provider,
        provider_is_loopback,
        "registration_endpoint",
        registration_endpoint,
    )?;

    match &metadata.code_challenge_methods_supported {
        None => Err(gate(
            provider,
            "authorization-server metadata has no code_challenge_methods_supported — the server does not support PKCE; refusing to proceed"
                .to_owned(),
        )),
        Some(methods) if !methods.iter().any(|m| m == "S256") => Err(gate(
            provider,
            format!(
                "authorization server supports PKCE methods {methods:?} but not S256; refusing to proceed"
            ),
        )),
        Some(_) => Ok(()),
    }
}

fn gate(provider: &str, message: String) -> PromptError {
    PromptError::OAuthValidation {
        provider: provider.to_owned(),
        message,
    }
}

fn is_loopback_host(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip
            .to_ipv4_mapped()
            .map_or_else(|| ip.is_loopback(), |v4| v4.is_loopback()),
        // The url crate lowercases domains; a trailing dot is the same name.
        Some(url::Host::Domain(domain)) => domain.trim_end_matches('.') == "localhost",
        None => false,
    }
}

/// Whether the host is an IP *literal* in a range that never denotes a public
/// server: loopback, RFC 1918 private, link-local (169.254/16 — where cloud
/// metadata endpoints live), CGNAT shared space (100.64/10), unspecified, or
/// the IPv6 equivalents (IPv4-mapped forms are classified through the IPv4
/// rules). This enumeration approximates the inverse of the still-unstable
/// `IpAddr::is_global()`; migrate to that when it stabilizes so new special
/// ranges land here automatically. Hostname filtering is deliberately out of
/// scope: without pinning DNS resolution it is trivially bypassed, so a public
/// name resolving to a private address is an accepted residual risk — the
/// HTTPS requirement forces such a target to present a valid certificate,
/// which defeats the drive-by case (internal services are rarely
/// TLS-certified for a public name), though not a determined attacker
/// pointing a validly-certified domain inward.
fn is_internal_ip_literal(url: &url::Url) -> bool {
    fn internal_v4(ip: std::net::Ipv4Addr) -> bool {
        let octets = ip.octets();
        ip.is_loopback()
            || ip.is_private()
            || ip.is_link_local()
            || ip.is_unspecified()
            || (octets[0] == 100 && (octets[1] & 0xc0) == 64) // shared 100.64/10
    }
    match url.host() {
        Some(url::Host::Ipv4(ip)) => internal_v4(ip),
        Some(url::Host::Ipv6(ip)) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return internal_v4(v4);
            }
            let segments = ip.segments();
            ip.is_loopback()
                || ip.is_unspecified()
                || (segments[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (segments[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
        Some(url::Host::Domain(domain)) => domain.trim_end_matches('.') == "localhost",
        None => false,
    }
}

/// The gate applied to every URL the OAuth machinery touches: the provider URL,
/// each discovery URL **before it is fetched** (a server must not be able to
/// point this client at arbitrary destinations — RFC 9728 §7.7's SSRF warning),
/// and the three endpoints where codes and tokens travel. Rules, in order:
///
/// - Only `http`/`https` schemes, ever.
/// - A loopback target is allowed **only when the provider itself is loopback**
///   (either scheme) — the all-local dev/test story stays intact, while a
///   remote server advertising a loopback destination is refused as the
///   nonsense it is.
/// - A non-loopback internal-range IP literal is refused for *everyone*, even
///   over HTTPS — see [`is_internal_ip_literal`] for the ranges and the
///   documented residual DNS risk.
/// - Everything else requires HTTPS.
fn require_safe_url(
    provider: &str,
    provider_is_loopback: bool,
    what: &str,
    url: &url::Url,
) -> Result<(), PromptError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(gate(
            provider,
            format!("{what} {url} has an unsupported scheme (only http/https)"),
        ));
    }
    if is_loopback_host(url) {
        return if provider_is_loopback {
            Ok(())
        } else {
            Err(gate(
                provider,
                format!(
                    "{what} {url} points at a loopback address; refusing for a remote provider"
                ),
            ))
        };
    }
    if is_internal_ip_literal(url) {
        return Err(gate(
            provider,
            format!("{what} {url} points at an internal address; refusing"),
        ));
    }
    if url.scheme() == "https" {
        Ok(())
    } else {
        Err(gate(
            provider,
            format!(
                "{what} {url} is not HTTPS (loopback http is allowed only for loopback providers)"
            ),
        ))
    }
}

fn require_safe_url_str(
    provider: &str,
    provider_is_loopback: bool,
    what: &str,
    url: &str,
) -> Result<(), PromptError> {
    let parsed =
        url::Url::parse(url).map_err(|e| gate(provider, format!("invalid {what} {url:?}: {e}")))?;
    require_safe_url(provider, provider_is_loopback, what, &parsed)
}

/// Probe the MCP endpoint unauthenticated and parse any `WWW-Authenticate`
/// challenge. Best-effort: an unreachable endpoint or absent header just means
/// no pointer and no scope challenge — the well-known fallbacks still run, and
/// a genuinely down server fails loudly at the metadata fetch instead.
async fn probe_challenge(http: &reqwest::Client, url: &str) -> Challenge {
    let response = match http.get(url).send().await {
        Ok(response) => response,
        Err(e) => {
            tracing::debug!(error = %e, "unauthenticated MCP probe failed; falling back to well-known discovery");
            return Challenge::default();
        }
    };
    let Some(header) = response
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
    else {
        return Challenge::default();
    };
    parse_www_authenticate(header)
}

/// Extract `resource_metadata` and `scope` from a `WWW-Authenticate` value like
/// `Bearer resource_metadata="https://…", scope="openid offline_access"`.
/// Deliberately a minimal parameter scan, not a full RFC 7235 parser — the two
/// parameters we need are simple quoted (or token) values. The raw header is
/// never logged (it is server-controlled text).
fn parse_www_authenticate(header: &str) -> Challenge {
    let mut challenge = Challenge::default();
    for (key, value) in scan_auth_params(header) {
        match key.as_str() {
            "resource_metadata" => challenge.resource_metadata = Some(value),
            "scope" => challenge.scope = Some(value),
            _ => {}
        }
    }
    challenge
}

/// Scan `key="value"` / `key=value` pairs from an auth-params string, skipping
/// the scheme word(s). Quoted values may contain commas and spaces.
fn scan_auth_params(header: &str) -> Vec<(String, String)> {
    let mut params = Vec::new();
    let mut rest = header;
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq]
            .rsplit([' ', ','])
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        rest = &rest[eq + 1..];
        let value = if let Some(stripped) = rest.strip_prefix('"') {
            let end = stripped.find('"').unwrap_or(stripped.len());
            let value = &stripped[..end];
            rest = stripped.get(end + 1..).unwrap_or_default();
            value.to_owned()
        } else {
            let end = rest.find([',', ' ']).unwrap_or(rest.len());
            let value = &rest[..end];
            rest = &rest[end..];
            value.to_owned()
        };
        if !key.is_empty() {
            params.push((key, value));
        }
    }
    params
}

/// RFC 9728 well-known candidates for the protected-resource metadata of a
/// resource with a path: the path-aware form first, then the origin root.
fn well_known_prm_urls(resource: &url::Url) -> Vec<String> {
    let origin = origin_of(resource);
    let path = resource.path().trim_end_matches('/');
    let mut candidates = vec![format!(
        "{origin}/.well-known/oauth-protected-resource{path}"
    )];
    if !path.is_empty() {
        candidates.push(format!("{origin}/.well-known/oauth-protected-resource"));
    }
    candidates
}

/// Authorization-server metadata candidates, in the exact order the MCP spec
/// mandates: OAuth 2.0 AS Metadata with path insertion, OIDC discovery with
/// path insertion, then — for path-bearing issuers only — OIDC discovery with
/// path *appending* (`{issuer}/.well-known/openid-configuration`, the form
/// Keycloak-style tenant issuers publish). For a path-less issuer the first
/// two forms are the complete spec list (insertion and appending coincide).
fn well_known_as_urls(issuer: &url::Url) -> Vec<String> {
    let origin = origin_of(issuer);
    let path = issuer.path().trim_end_matches('/');
    let mut candidates = vec![
        format!("{origin}/.well-known/oauth-authorization-server{path}"),
        format!("{origin}/.well-known/openid-configuration{path}"),
    ];
    if !path.is_empty() {
        candidates.push(format!("{origin}{path}/.well-known/openid-configuration"));
    }
    candidates
}

fn origin_of(url: &url::Url) -> String {
    let scheme = url.scheme();
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
    }
}

/// Fetch the first candidate URL that returns HTTP 200 with a body that parses
/// as `T`. If none does, the error lists every attempt so the failure
/// diagnoses itself.
async fn fetch_first_json<T: DeserializeOwned>(
    http: &reqwest::Client,
    provider: &str,
    what: &str,
    candidates: &[String],
) -> Result<T, PromptError> {
    let mut attempts = Vec::new();
    for candidate in candidates {
        match fetch_json::<T>(http, candidate).await {
            Ok(value) => return Ok(value),
            Err(reason) => attempts.push(format!("{candidate}: {reason}")),
        }
    }
    Err(gate(
        provider,
        format!("could not fetch {what} (tried {})", attempts.join("; ")),
    ))
}

async fn fetch_json<T: DeserializeOwned>(http: &reqwest::Client, url: &str) -> Result<T, String> {
    let response = http.get(url).send().await.map_err(|e| e.to_string())?;
    // The shared client refuses redirects (`Policy::none` — a 3xx could
    // launder an approved URL into an internal or http destination). Some
    // servers legitimately redirect their well-known endpoints, so name the
    // refusal and the target explicitly rather than leaving a bare "HTTP 301"
    // for the operator to puzzle over.
    if response.status().is_redirection() {
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("<no Location header>");
        return Err(format!(
            "HTTP {} — redirects are refused during discovery (Location: {location})",
            response.status()
        ));
    }
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let body = response.text().await.map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| format!("unparseable response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn parses_www_authenticate_params() {
        let challenge = parse_www_authenticate(
            r#"Bearer resource_metadata="https://h/.well-known/oauth-protected-resource/mcp", scope="openid offline_access""#,
        );
        assert_eq!(
            challenge.resource_metadata.as_deref(),
            Some("https://h/.well-known/oauth-protected-resource/mcp")
        );
        assert_eq!(challenge.scope.as_deref(), Some("openid offline_access"));

        // Unquoted values and absent params degrade cleanly.
        let challenge = parse_www_authenticate("Bearer realm=api, scope=openid");
        assert!(challenge.resource_metadata.is_none());
        assert_eq!(challenge.scope.as_deref(), Some("openid"));

        let challenge = parse_www_authenticate("Bearer");
        assert!(challenge.resource_metadata.is_none());
        assert!(challenge.scope.is_none());
    }

    #[test]
    fn well_known_candidates_honor_paths() {
        let resource = url::Url::parse("https://mcp.example.com/mcp").unwrap();
        assert_eq!(
            well_known_prm_urls(&resource),
            vec![
                "https://mcp.example.com/.well-known/oauth-protected-resource/mcp".to_owned(),
                "https://mcp.example.com/.well-known/oauth-protected-resource".to_owned(),
            ]
        );
        // Path-bearing issuer: the MCP spec's full three-candidate list, in
        // order — the path-appended OIDC form is what Keycloak-style tenant
        // issuers publish.
        let issuer = url::Url::parse("https://as.example.com:8443/tenant").unwrap();
        assert_eq!(
            well_known_as_urls(&issuer),
            vec![
                "https://as.example.com:8443/.well-known/oauth-authorization-server/tenant"
                    .to_owned(),
                "https://as.example.com:8443/.well-known/openid-configuration/tenant".to_owned(),
                "https://as.example.com:8443/tenant/.well-known/openid-configuration".to_owned(),
            ]
        );
        // Path-less issuer: insertion and appending coincide; two candidates.
        let bare = url::Url::parse("https://as.example.com").unwrap();
        assert_eq!(well_known_as_urls(&bare).len(), 2);
    }

    #[test]
    fn url_safety_gate_rules() {
        let check = |provider_loopback: bool, url: &str| {
            require_safe_url(
                "team",
                provider_loopback,
                "test URL",
                &url::Url::parse(url).unwrap(),
            )
        };
        // Public HTTPS is fine for anyone.
        assert!(check(false, "https://as.example.com/x").is_ok());
        assert!(check(true, "https://as.example.com/x").is_ok());
        // http is only for loopback hosts of loopback providers.
        assert!(check(false, "http://as.example.com/x").is_err());
        assert!(check(true, "http://127.0.0.1:9/x").is_ok());
        assert!(check(true, "http://[::1]:9/x").is_ok());
        assert!(check(false, "http://127.0.0.1:9/x").is_err());
        // Internal-range literals are refused even over HTTPS — including the
        // cloud-metadata link-local range, CGNAT shared space, and IPv4-mapped
        // IPv6 spellings — and (non-loopback ones) for loopback providers too:
        // the exemption is loopback-to-loopback, not loopback-to-anywhere.
        assert!(check(false, "https://192.168.1.1/x").is_err());
        assert!(check(true, "https://192.168.1.1/x").is_err());
        assert!(check(false, "https://10.0.0.1/x").is_err());
        assert!(check(true, "https://10.0.0.1/x").is_err());
        assert!(check(false, "https://169.254.169.254/latest/meta-data").is_err());
        assert!(check(false, "https://100.64.0.1/x").is_err());
        assert!(check(false, "https://[::ffff:169.254.169.254]/x").is_err());
        assert!(check(false, "https://[::ffff:10.0.0.1]/x").is_err());
        assert!(check(false, "https://localhost/x").is_err());
        assert!(check(false, "https://localhost./x").is_err());
        assert!(check(false, "https://[fe80::1]/x").is_err());
        assert!(check(false, "https://[fc00::1]/x").is_err());
        // Only http/https, even toward loopback.
        assert!(check(true, "ftp://127.0.0.1/x").is_err());
        // ...but a loopback provider may talk to its own machine.
        assert!(check(true, "http://localhost:9/x").is_ok());
        assert!(check(true, "http://localhost.:9/x").is_ok());
        assert!(check(true, "https://[::ffff:127.0.0.1]/x").is_ok());
    }

    /// What the fake server serves. Built after bind so bodies can embed the
    /// ephemeral base URL.
    struct Spec {
        www_authenticate: Option<String>,
        prm: Option<serde_json::Value>,
        /// When set, the PRM route answers 307 → this Location instead of JSON.
        prm_redirects_to: Option<String>,
        as_metadata: Option<serde_json::Value>,
    }

    #[derive(Default)]
    struct Counters {
        prm_fetches: AtomicUsize,
        as_fetches: AtomicUsize,
    }

    struct Fake {
        base: String,
        counters: Arc<Counters>,
    }

    impl Fake {
        fn mcp_url(&self) -> String {
            format!("{}/mcp", self.base)
        }
    }

    /// A fully-conformant server spec; negatives are built by mutating it.
    fn happy_spec(base: &str) -> Spec {
        Spec {
            www_authenticate: Some(format!(
                r#"Bearer resource_metadata="{base}/.well-known/oauth-protected-resource/mcp""#
            )),
            prm: Some(json!({
                "resource": format!("{base}/mcp"),
                "authorization_servers": [base],
                "scopes_supported": ["openid", "offline_access"],
            })),
            prm_redirects_to: None,
            as_metadata: Some(json!({
                "issuer": base,
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"),
                "registration_endpoint": format!("{base}/register"),
                "code_challenge_methods_supported": ["S256"],
            })),
        }
    }

    type Ctx = (Arc<Spec>, Arc<Counters>);

    async fn mcp(
        axum::extract::State((spec, _)): axum::extract::State<Ctx>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        let mut response = axum::http::StatusCode::UNAUTHORIZED.into_response();
        if let Some(header) = &spec.www_authenticate {
            response
                .headers_mut()
                .insert("www-authenticate", header.parse().unwrap());
        }
        response
    }

    async fn prm(
        axum::extract::State((spec, counters)): axum::extract::State<Ctx>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        counters.prm_fetches.fetch_add(1, Ordering::SeqCst);
        if let Some(location) = &spec.prm_redirects_to {
            let mut response = axum::http::StatusCode::TEMPORARY_REDIRECT.into_response();
            response
                .headers_mut()
                .insert("location", location.parse().unwrap());
            return response;
        }
        match &spec.prm {
            Some(body) => axum::Json(body.clone()).into_response(),
            None => axum::http::StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn asm(
        axum::extract::State((spec, counters)): axum::extract::State<Ctx>,
    ) -> axum::response::Response {
        use axum::response::IntoResponse;
        counters.as_fetches.fetch_add(1, Ordering::SeqCst);
        match &spec.as_metadata {
            Some(body) => axum::Json(body.clone()).into_response(),
            None => axum::http::StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn spawn(build: impl FnOnce(&str) -> Spec) -> Fake {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let spec = Arc::new(build(&base));
        let counters = Arc::new(Counters::default());

        let router = axum::Router::new()
            .route("/mcp", axum::routing::get(mcp))
            .route(
                "/.well-known/oauth-protected-resource/mcp",
                axum::routing::get(prm),
            )
            .route(
                "/.well-known/oauth-protected-resource",
                axum::routing::get(prm),
            )
            .route(
                "/.well-known/oauth-authorization-server",
                axum::routing::get(asm),
            )
            .with_state((spec, counters.clone()));
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Fake { base, counters }
    }

    /// Mirrors the service's shared-client shape: redirects refused, so these
    /// tests exercise the same policy production runs under.
    fn http() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    async fn run(
        fake: &Fake,
        scopes_override: Option<&[String]>,
    ) -> Result<PreflightOutcome, PromptError> {
        preflight(&http(), "team", &fake.mcp_url(), scopes_override).await
    }

    #[tokio::test]
    async fn conformant_server_passes_and_resolves_scopes_from_prm() {
        let fake = spawn(happy_spec).await;
        let outcome = run(&fake, None).await.unwrap();
        assert_eq!(
            outcome.metadata.token_endpoint,
            format!("{}/token", fake.base)
        );
        assert_eq!(
            outcome.scopes,
            Some(vec!["openid".to_owned(), "offline_access".to_owned()])
        );
        // The challenge pointer was used; only one PRM fetch was needed.
        assert_eq!(fake.counters.prm_fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn falls_back_to_well_known_when_no_challenge_header() {
        let fake = spawn(|base| Spec {
            www_authenticate: None,
            ..happy_spec(base)
        })
        .await;
        assert!(run(&fake, None).await.is_ok());
    }

    #[tokio::test]
    async fn scope_priority_override_beats_challenge_beats_prm() {
        // Server advertises a scope challenge AND PRM scopes_supported.
        let with_challenge = |base: &str| Spec {
            www_authenticate: Some(format!(
                r#"Bearer resource_metadata="{base}/.well-known/oauth-protected-resource/mcp", scope="from-challenge""#
            )),
            ..happy_spec(base)
        };

        // Config override wins over everything.
        let fake = spawn(with_challenge).await;
        let outcome = run(&fake, Some(&["from-config".to_owned()])).await.unwrap();
        assert_eq!(outcome.scopes, Some(vec!["from-config".to_owned()]));

        // No override: the 401 challenge beats the PRM list.
        let fake = spawn(with_challenge).await;
        let outcome = run(&fake, None).await.unwrap();
        assert_eq!(outcome.scopes, Some(vec!["from-challenge".to_owned()]));

        // No override, no challenge scope: PRM scopes_supported.
        let fake = spawn(happy_spec).await;
        let outcome = run(&fake, None).await.unwrap();
        assert_eq!(
            outcome.scopes,
            Some(vec!["openid".to_owned(), "offline_access".to_owned()])
        );

        // Nothing anywhere: omit the scope parameter entirely.
        let fake = spawn(|base| {
            let mut spec = happy_spec(base);
            spec.prm = Some(json!({
                "resource": format!("{base}/mcp"),
                "authorization_servers": [base],
            }));
            spec
        })
        .await;
        let outcome = run(&fake, None).await.unwrap();
        assert_eq!(outcome.scopes, None);
    }

    #[tokio::test]
    async fn rejects_non_https_provider_url_before_any_request() {
        // A routable (non-loopback) http URL fails the very first gate — no
        // server exists at this address, proving nothing was fetched.
        let err = preflight(&http(), "team", "http://192.0.2.1/mcp", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not HTTPS"), "{err}");
    }

    #[tokio::test]
    async fn rejects_resource_mismatch_with_both_values() {
        let fake = spawn(|base| {
            let mut spec = happy_spec(base);
            spec.prm = Some(json!({
                "resource": "https://other.example.com/mcp",
                "authorization_servers": [base],
            }));
            spec
        })
        .await;
        let err = run(&fake, None).await.unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("https://other.example.com/mcp"),
            "{message}"
        );
        assert!(message.contains(&fake.mcp_url()), "{message}");
    }

    #[tokio::test]
    async fn rejects_non_https_endpoints() {
        for endpoint in [
            "authorization_endpoint",
            "token_endpoint",
            "registration_endpoint",
        ] {
            let fake = spawn(move |base| {
                let mut spec = happy_spec(base);
                let asm = spec.as_metadata.as_mut().unwrap();
                // Loopback http is exempt, so use a routable http URL.
                asm[endpoint] = json!("http://as.example.com/x");
                spec
            })
            .await;
            let err = run(&fake, None).await.unwrap_err();
            let message = err.to_string();
            assert!(message.contains(endpoint), "{message}");
            assert!(message.contains("not HTTPS"), "{message}");
        }
    }

    #[tokio::test]
    async fn rejects_missing_registration_endpoint() {
        let fake = spawn(|base| {
            let mut spec = happy_spec(base);
            spec.as_metadata
                .as_mut()
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("registration_endpoint");
            spec
        })
        .await;
        let err = run(&fake, None).await.unwrap_err();
        assert!(
            err.to_string().contains("dynamic client registration"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn rejects_absent_pkce_and_pkce_without_s256() {
        let fake = spawn(|base| {
            let mut spec = happy_spec(base);
            spec.as_metadata
                .as_mut()
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("code_challenge_methods_supported");
            spec
        })
        .await;
        let err = run(&fake, None).await.unwrap_err();
        assert!(err.to_string().contains("does not support PKCE"), "{err}");

        let fake = spawn(|base| {
            let mut spec = happy_spec(base);
            spec.as_metadata.as_mut().unwrap()["code_challenge_methods_supported"] =
                json!(["plain"]);
            spec
        })
        .await;
        let err = run(&fake, None).await.unwrap_err();
        assert!(err.to_string().contains("not S256"), "{err}");
    }

    #[tokio::test]
    async fn rejects_provider_url_with_fragment_before_any_request() {
        // Port 1 is closed, so a request would fail loudly — the fragment gate
        // fires first, proving nothing was fetched.
        let err = preflight(&http(), "team", "http://127.0.0.1:1/mcp#frag", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("fragment"), "{err}");
    }

    #[tokio::test]
    async fn unsafe_challenge_pointer_is_ignored_and_fallback_used() {
        // The 401 challenge points at a non-loopback http URL: it must be
        // filtered (never fetched) and the well-known fallback must carry the
        // discovery instead — exactly one PRM fetch, all against our origin.
        let fake = spawn(|base| Spec {
            www_authenticate: Some(
                r#"Bearer resource_metadata="http://attacker.example.com/prm""#.to_owned(),
            ),
            ..happy_spec(base)
        })
        .await;
        assert!(run(&fake, None).await.is_ok());
        assert_eq!(fake.counters.prm_fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn redirecting_discovery_endpoint_is_refused_with_a_named_location() {
        let fake = spawn(|base| Spec {
            prm_redirects_to: Some(format!("{base}/somewhere-else")),
            ..happy_spec(base)
        })
        .await;
        let err = run(&fake, None).await.unwrap_err();
        let message = err.to_string();
        assert!(message.contains("redirects are refused"), "{message}");
        assert!(message.contains("somewhere-else"), "{message}");
    }

    #[tokio::test]
    async fn advertised_resource_is_canonicalized_before_comparison() {
        // A conformant server may spell the resource with an uppercase scheme/
        // host; that must match, not hard-refuse with two identical-looking
        // strings (the inverse of the config-side canonicalization test).
        let fake = spawn(|base| {
            let mut spec = happy_spec(base);
            let shouty = format!("{base}/mcp").replacen("http://", "HTTP://", 1);
            spec.prm.as_mut().unwrap()["resource"] = json!(shouty);
            spec
        })
        .await;
        assert!(run(&fake, None).await.is_ok());
    }

    #[tokio::test]
    async fn missing_resource_field_names_the_omission() {
        let fake = spawn(|base| {
            let mut spec = happy_spec(base);
            spec.prm
                .as_mut()
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("resource");
            spec
        })
        .await;
        let err = run(&fake, None).await.unwrap_err();
        assert!(err.to_string().contains("omits the required"), "{err}");
    }

    #[tokio::test]
    async fn issuer_binding_rejects_missing_and_mismatched_issuers() {
        // RFC 8414 §3.3: the metadata must claim exactly the identity used for
        // discovery — the mix-up-attack defense.
        let fake = spawn(|base| {
            let mut spec = happy_spec(base);
            spec.as_metadata
                .as_mut()
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove("issuer");
            spec
        })
        .await;
        let err = run(&fake, None).await.unwrap_err();
        assert!(
            err.to_string().contains("omits the required `issuer`"),
            "{err}"
        );

        let fake = spawn(|base| {
            let mut spec = happy_spec(base);
            spec.as_metadata.as_mut().unwrap()["issuer"] = json!("http://127.0.0.1:1");
            spec
        })
        .await;
        let err = run(&fake, None).await.unwrap_err();
        let message = err.to_string();
        assert!(message.contains("claims issuer"), "{message}");
        assert!(message.contains("http://127.0.0.1:1"), "{message}");
    }

    #[tokio::test]
    async fn issuer_binding_tolerates_trailing_slash_differences() {
        // The PRM advertises the AS with a trailing slash; the server's issuer
        // has none. Same identity — must pass, because discovery candidates
        // and the issuer comparison share one derivation (`as_identifier`).
        // Comparing the canonical form (which keeps the slash) against the
        // trimmed discovery form would refuse a conformant server over one
        // invisible character.
        let fake = spawn(|base| {
            let mut spec = happy_spec(base);
            spec.prm.as_mut().unwrap()["authorization_servers"] = json!([format!("{base}/")]);
            spec
        })
        .await;
        assert!(run(&fake, None).await.is_ok());
    }

    #[tokio::test]
    async fn issuer_with_query_is_rejected() {
        let fake = spawn(|base| {
            let mut spec = happy_spec(base);
            spec.prm.as_mut().unwrap()["authorization_servers"] = json!([format!("{base}?x=1")]);
            spec
        })
        .await;
        let err = run(&fake, None).await.unwrap_err();
        assert!(
            err.to_string().contains("must not contain a query"),
            "{err}"
        );
    }
}
