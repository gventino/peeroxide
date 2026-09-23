//! HTTPS requests to GitHub (or a local test server).

use std::time::Duration;

use ureq::tls::{RootCerts, TlsConfig};

use crate::{CHECK_TIMEOUT, UpdateError};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RELEASES_LIMIT: u64 = 1024 * 1024;

/// Only HTTPS, except plain HTTP to this computer for local test servers.
pub(crate) fn check_url(url: &str) -> Result<(), UpdateError> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split(['/', ':']).next().unwrap_or_default();
        if host == "127.0.0.1" || host == "localhost" {
            return Ok(());
        }
    }
    Err(UpdateError::InsecureUrl(url.to_string()))
}

/// An HTTP client for `url`. HTTPS stays HTTPS through redirects and checks certificates
/// against the operating system's store (which also covers antivirus HTTPS inspection).
pub(crate) fn agent(url: &str, timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .https_only(url.starts_with("https://"))
        .http_status_as_error(false)
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

pub(crate) fn network_error(e: ureq::Error) -> UpdateError {
    match e {
        ureq::Error::BodyExceedsLimit(_) => UpdateError::TooLarge,
        other => UpdateError::Network(other.to_string()),
    }
}

/// `Ok` for 200; a clear error for anything else.
pub(crate) fn expect_ok(status: u16) -> Result<(), UpdateError> {
    match status {
        200 => Ok(()),
        403 | 429 => Err(UpdateError::RateLimited),
        code => Err(UpdateError::BadResponse(format!("HTTP {code}"))),
    }
}

/// The raw JSON list of releases, newest first.
pub(crate) fn fetch(api_url: &str, user_agent: &str) -> Result<Vec<u8>, UpdateError> {
    check_url(api_url)?;
    let url = if api_url.contains('?') {
        api_url.to_string()
    } else {
        format!("{api_url}?per_page=20")
    };
    let mut response = agent(&url, CHECK_TIMEOUT)
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", user_agent)
        .call()
        .map_err(network_error)?;
    expect_ok(response.status().as_u16())?;
    response
        .body_mut()
        .with_config()
        .limit(RELEASES_LIMIT)
        .read_to_vec()
        .map_err(network_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_https_or_this_computer() {
        assert!(check_url("https://api.github.com/repos/x/y/releases").is_ok());
        assert!(check_url("http://127.0.0.1:8080/releases").is_ok());
        assert!(check_url("http://localhost/releases").is_ok());
        assert!(check_url("http://api.github.com/releases").is_err());
        assert!(check_url("http://127.0.0.1.evil.example/releases").is_err());
        assert!(check_url("ftp://example.com").is_err());
    }
}
