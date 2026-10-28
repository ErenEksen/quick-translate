//! Transport rules: HTTPS everywhere except loopback, unless the user confirms (spec: transport
//! security and privacy).
//!
//! The app has no telemetry: the only requests made are to the active backend's endpoint and
//! the DeepL usage endpoint, and redirects are never followed (see [`crate::http`]).

use std::net::IpAddr;

use url::{Host, Url};

/// Why an endpoint URL was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndpointError {
    #[error("Invalid URL: {0}")]
    Invalid(String),
    #[error("Only http:// and https:// URLs are supported")]
    UnsupportedScheme,
    /// Plain `http://` to a non-loopback host. Settings must ask the user to confirm.
    #[error(
        "This endpoint uses an unencrypted http:// connection. Confirm to send text over it anyway."
    )]
    NeedsConfirmation,
}

impl From<EndpointError> for crate::Error {
    fn from(e: EndpointError) -> Self {
        match e {
            EndpointError::NeedsConfirmation => Self::InsecureEndpoint,
            EndpointError::Invalid(_) | EndpointError::UnsupportedScheme => Self::NotConfigured,
        }
    }
}

/// Parses and checks a base URL.
///
/// `https://` is always accepted. `http://` is accepted for loopback hosts (`127.0.0.0/8`,
/// `::1`, `localhost` and `*.localhost`), and for other hosts only when `insecure_confirmed`.
pub fn check_endpoint(raw: &str, insecure_confirmed: bool) -> Result<Url, EndpointError> {
    let url = Url::parse(raw.trim()).map_err(|e| EndpointError::Invalid(e.to_string()))?;
    if url.host().is_none() {
        return Err(EndpointError::Invalid("missing host".into()));
    }
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_loopback(&url) || insecure_confirmed => Ok(url),
        "http" => Err(EndpointError::NeedsConfirmation),
        _ => Err(EndpointError::UnsupportedScheme),
    }
}

/// Whether the URL's host is a loopback address or name.
pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
        Some(Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            name == "localhost"
                || name.ends_with(".localhost")
                || name.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(raw: &str) -> bool {
        check_endpoint(raw, false).is_ok()
    }

    #[test]
    fn loopback_http_is_accepted() {
        assert!(ok("http://127.0.0.1:8080/v1"));
        assert!(ok("http://127.1.2.3/v1"));
        assert!(ok("http://[::1]:11434/v1"));
        assert!(ok("http://[::ffff:127.0.0.1]/v1"));
        assert!(ok("http://localhost:11434/v1"));
        assert!(ok("http://LOCALHOST/v1"));
        assert!(ok("http://localhost./v1"));
        assert!(ok("http://ollama.localhost:11434/v1"));
    }

    #[test]
    fn remote_http_needs_confirmation() {
        for raw in [
            "http://192.168.1.20:11434/v1",
            "http://10.0.0.5/v1",
            "http://[fe80::1]/v1",
            "http://example.com/v1",
            "http://localhost.example.com/v1",
            "http://0.0.0.0/v1",
        ] {
            assert_eq!(
                check_endpoint(raw, false),
                Err(EndpointError::NeedsConfirmation),
                "{raw}"
            );
            assert!(check_endpoint(raw, true).is_ok(), "{raw} with confirmation");
        }
    }

    #[test]
    fn https_is_always_accepted() {
        assert!(ok("https://api.openai.com/v1"));
        assert!(ok("https://192.168.1.20/v1"));
        assert!(ok("https://localhost/v1"));
    }

    #[test]
    fn rejects_other_schemes_and_garbage() {
        assert_eq!(
            check_endpoint("ftp://example.com", true),
            Err(EndpointError::UnsupportedScheme)
        );
        assert_eq!(check_endpoint("file:///etc/passwd", true).ok(), None);
        assert!(matches!(
            check_endpoint("not a url", true),
            Err(EndpointError::Invalid(_))
        ));
        assert!(matches!(
            check_endpoint("", false),
            Err(EndpointError::Invalid(_))
        ));
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(
            check_endpoint("  https://api.openai.com/v1 \n", false)
                .unwrap()
                .as_str(),
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn maps_to_backend_errors() {
        assert_eq!(
            crate::Error::from(EndpointError::NeedsConfirmation),
            crate::Error::InsecureEndpoint
        );
        assert_eq!(
            crate::Error::from(EndpointError::UnsupportedScheme),
            crate::Error::NotConfigured
        );
    }
}
