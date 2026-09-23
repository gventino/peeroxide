//! Downloads with hard size limits, progress and cancellation.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::UpdateError;
use crate::github::{agent, check_url, expect_ok, network_error};
use crate::install::writable_error;

const SMALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Safety net only: the user can skip a slow download at any time.
const PACKAGE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// A small text file, such as a `.minisig` signature.
pub(crate) fn to_string(url: &str, limit: u64, user_agent: &str) -> Result<String, UpdateError> {
    check_url(url)?;
    let mut response = agent(url, SMALL_TIMEOUT)
        .get(url)
        .header("User-Agent", user_agent)
        .call()
        .map_err(network_error)?;
    expect_ok(response.status().as_u16())?;
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_string()
        .map_err(network_error)
}

/// Streams `url` into `dest`, which must end up exactly `expected` bytes long (the size GitHub
/// lists for the asset). `progress` gets the running total.
pub(crate) fn to_file(
    url: &str,
    dest: &Path,
    limit: u64,
    expected: u64,
    user_agent: &str,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<(), UpdateError> {
    check_url(url)?;
    if expected > limit {
        return Err(UpdateError::TooLarge);
    }
    let response = agent(url, PACKAGE_TIMEOUT)
        .get(url)
        .header("User-Agent", user_agent)
        .call()
        .map_err(network_error)?;
    expect_ok(response.status().as_u16())?;
    let mut body = response
        .into_body()
        .into_with_config()
        .limit(expected + 1)
        .reader();
    let mut file = std::fs::File::create(dest).map_err(|e| writable_error(&e))?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(UpdateError::Cancelled);
        }
        let n = body
            .read(&mut buf)
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > expected {
            return Err(UpdateError::TooLarge);
        }
        file.write_all(&buf[..n]).map_err(|e| writable_error(&e))?;
        progress(done);
    }
    if done != expected {
        return Err(UpdateError::Package(format!(
            "download stopped at {done} of {expected} bytes"
        )));
    }
    file.flush().map_err(|e| writable_error(&e))
}
