//! Blocking HTTP client with on-disk caching and conditional GET.
//!
//! Backed by [`ureq`] (no async, no tokio). Uses native env-var
//! support for HTTP_PROXY / HTTPS_PROXY / NO_PROXY. Each fetched
//! body is cached under `~/.cache/rpm-spec-tool/http/<sha-url>/`
//! with ETag and Last-Modified sidecar so subsequent fetches can
//! short-circuit on `304 Not Modified`.

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use rpm_spec_repo_core::{HttpError, RepoError};

use crate::util::{atomic_write, sha256_hex};

/// URL schemes that [`HttpCache::fetch`] is allowed to dial. SSRF
/// guard rail: rejects `file://`, `ftp://`, `gopher://`, and any
/// other surprise scheme that a hostile `.rpmspec.toml` might try to
/// smuggle in.
pub const ALLOWED_SCHEMES: &[&str] = &["http", "https"];

/// Upper bound on the raw HTTP response body the cache will buffer
/// into memory. 512 MiB is large enough for any real-world
/// `primary.xml.gz` / `filelists.xml.gz` (Fedora Everything's
/// largest is ~200 MiB) but small enough to keep a single hostile
/// fetch from exhausting the process heap. Decompression is bounded
/// separately by `compression::MAX_DECOMPRESSED_BYTES`.
pub const MAX_RESPONSE_BYTES: u64 = 512 * 1024 * 1024;

/// Maximum number of HTTP redirects to follow before erroring. Caps
/// SSRF-style redirect chains and accidental loops on misconfigured
/// mirrors.
pub const MAX_REDIRECTS: u32 = 5;

/// Network access policy.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NetMode {
    /// Default. Cache-only — refuses network fetches with
    /// [`HttpError::OfflinePolicy`].
    Offline,
    /// Like Offline but errors on cache miss too. CI guard rail.
    CacheOnly,
    /// Network allowed.
    Online,
}

impl NetMode {
    /// Whether this mode permits an HTTP request. `CacheOnly` is
    /// distinct from `Offline` in error semantics (cache miss →
    /// error vs. silent skip) but neither sends bytes.
    #[must_use]
    pub fn allows_network(self) -> bool {
        matches!(self, Self::Online)
    }
}

/// On-disk + in-memory HTTP cache. One per process; clone-cheap
/// (internal state is `Arc`-wrapped — for M1 we keep it simple and
/// use the disk directly).
#[derive(Debug, Clone)]
pub struct HttpCache {
    root: PathBuf,
    mode: NetMode,
    agent: ureq::Agent,
}

/// Sidecar metadata for a cached response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HttpCacheMeta {
    url: String,
    etag: Option<String>,
    last_modified: Option<String>,
    fetched_at_unix: u64,
    body_sha256: String,
}

impl HttpCache {
    /// Construct rooted at `~/.cache/rpm-spec-tool/http/` (or override
    /// via the `cache_root` constructor argument so tests can scope
    /// to a tempdir).
    ///
    /// # Security
    ///
    /// URLs handed to [`HttpCache::fetch`] typically originate from
    /// user-supplied `.rpmspec.toml` files. Callers running with
    /// `--allow-fetch` MUST review those configs before invocation —
    /// the cache enforces a scheme allowlist (see [`ALLOWED_SCHEMES`])
    /// and a redirect cap ([`MAX_REDIRECTS`]) but cannot itself
    /// distinguish an intended mirror from a hostile one.
    pub fn new(cache_root: PathBuf, mode: NetMode) -> Result<Self, RepoError> {
        Self::new_with_tls(cache_root, mode, true)
    }

    /// Like [`HttpCache::new`] but lets the caller bypass TLS
    /// certificate verification when `verify_tls = false`.
    ///
    /// # Security
    ///
    /// Disabling verification trusts ANY server identity on HTTPS —
    /// strip-and-replay, MITM, and DNS-hijack attacks all succeed
    /// silently. Reserve for internal corporate mirrors whose CA you
    /// haven't (yet) installed in the system trust store, and only
    /// when you've vetted the network path. Production CI must keep
    /// `verify_tls = true`.
    pub fn new_with_tls(
        cache_root: PathBuf,
        mode: NetMode,
        verify_tls: bool,
    ) -> Result<Self, RepoError> {
        let root = cache_root.join("http");
        fs::create_dir_all(&root)?;
        // Native env-var proxy: ureq honours `HTTP_PROXY` /
        // `HTTPS_PROXY` / `NO_PROXY` automatically; no extra wiring
        // needed inside a corporate network.
        let mut builder = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(30))
            .redirects(MAX_REDIRECTS)
            .user_agent(concat!("rpm-spec-tool/", env!("CARGO_PKG_VERSION")));
        if !verify_tls {
            tracing::warn!(
                "TLS certificate verification disabled — any server identity will be accepted"
            );
            builder = builder.tls_config(std::sync::Arc::new(insecure_rustls_config()));
        }
        let agent = builder.build();
        Ok(Self { root, mode, agent })
    }

    #[must_use]
    pub fn mode(&self) -> NetMode {
        self.mode
    }

    /// Fetch `url`, respecting cache and `mode`. Returns the body
    /// bytes (decompressed transport-encoding only — content-level
    /// compression like gzip primary.xml.gz is handled by the
    /// caller).
    pub fn fetch(&self, url: &str) -> Result<Vec<u8>, RepoError> {
        // SSRF guard: validate the URL scheme before we even touch
        // the cache directory or attempt to dial out.
        let scheme = url
            .split_once("://")
            .map(|(s, _)| s.to_ascii_lowercase())
            .ok_or_else(|| {
                RepoError::Http(HttpError::InvalidUrl(format!(
                    "missing scheme in URL `{url}`; allowed: {ALLOWED_SCHEMES:?}"
                )))
            })?;
        if !ALLOWED_SCHEMES.iter().any(|s| *s == scheme) {
            return Err(RepoError::Http(HttpError::InvalidUrl(format!(
                "unsupported scheme `{scheme}` in {url}; allowed: {ALLOWED_SCHEMES:?}"
            ))));
        }

        let key = sha256_hex(url.as_bytes());
        let dir = self.root.join(&key);
        let body_path = dir.join("body");
        let meta_path = dir.join("meta.json");

        let cached_meta: Option<HttpCacheMeta> = if meta_path.exists() {
            match fs::read_to_string(&meta_path) {
                Ok(s) => match serde_json::from_str(&s) {
                    Ok(m) => Some(m),
                    Err(e) => {
                        tracing::warn!(
                            path = ?meta_path,
                            error = %e,
                            "corrupt http cache sidecar — refetching"
                        );
                        None
                    }
                },
                Err(_) => None,
            }
        } else {
            None
        };

        if !self.mode.allows_network() {
            return if body_path.exists() {
                Ok(fs::read(&body_path)?)
            } else {
                Err(RepoError::Http(HttpError::OfflinePolicy {
                    url: url.to_string(),
                }))
            };
        }

        // Use Debug formatting (`?url`) instead of Display (`%url`) so
        // control characters in a hostile URL are escaped before they
        // hit structured log lines.
        let span = tracing::info_span!("http.fetch", url = ?url);
        let _g = span.enter();

        let mut req = self.agent.get(url);
        if let Some(m) = &cached_meta {
            if let Some(etag) = &m.etag {
                req = req.set("If-None-Match", etag);
            }
            if let Some(lm) = &m.last_modified {
                req = req.set("If-Modified-Since", lm);
            }
        }

        let response = req.call().map_err(|e| {
            RepoError::Http(HttpError::Network {
                url: url.to_string(),
                source: Box::new(e),
            })
        })?;

        if response.status() == 304 {
            tracing::debug!(url = ?url, "304 Not Modified — using cache");
            return Ok(fs::read(&body_path)?);
        }

        if !(200..300).contains(&response.status()) {
            return Err(RepoError::Http(HttpError::Status {
                url: url.to_string(),
                status: response.status(),
            }));
        }

        let etag = response.header("etag").map(|s| s.to_string());
        let last_modified = response.header("last-modified").map(|s| s.to_string());

        let mut body = Vec::new();
        response
            .into_reader()
            .take(MAX_RESPONSE_BYTES)
            .read_to_end(&mut body)
            .map_err(|e| {
                RepoError::Http(HttpError::Network {
                    url: url.to_string(),
                    source: Box::new(e),
                })
            })?;

        let body_sha256 = sha256_hex(&body);
        fs::create_dir_all(&dir)?;
        atomic_write(&body_path, &body)?;
        let meta = HttpCacheMeta {
            url: url.to_string(),
            etag,
            last_modified,
            fetched_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            body_sha256,
        };
        let meta_json = serde_json::to_string_pretty(&meta)
            .map_err(|e| RepoError::Serialize(e.to_string()))?;
        atomic_write(&meta_path, meta_json.as_bytes())?;

        Ok(body)
    }
}

/// Build a rustls config that accepts any server certificate.
///
/// Strictly for `--insecure-tls` invocations against internal mirrors
/// whose CA isn't in the system trust store. Never call from any
/// other path.
fn insecure_rustls_config() -> rustls::ClientConfig {
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| std::sync::Arc::new(rustls::crypto::ring::default_provider()));
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("safe default rustls protocol versions are always available")
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(NoVerifier))
        .with_no_client_auth()
}

/// rustls `ServerCertVerifier` that accepts every certificate. See
/// [`insecure_rustls_config`] for the only justified caller.
#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        use rustls::SignatureScheme::*;
        vec![
            RSA_PKCS1_SHA256,
            RSA_PKCS1_SHA384,
            RSA_PKCS1_SHA512,
            ECDSA_NISTP256_SHA256,
            ECDSA_NISTP384_SHA384,
            ECDSA_NISTP521_SHA512,
            RSA_PSS_SHA256,
            RSA_PSS_SHA384,
            RSA_PSS_SHA512,
            ED25519,
            ED448,
        ]
    }
}
