//! Which release to install: the newest one that is strictly newer than the running version and
//! publishes a signed package for this platform.

use semver::Version;
use serde::Deserialize;

use crate::UpdateError;

/// One entry of GitHub's "list releases" answer (only the fields used).
#[derive(Clone, Debug, Deserialize)]
pub struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Clone, Debug, Deserialize)]
struct GhAsset {
    name: String,
    size: u64,
    browser_download_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub size: u64,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    /// The release page, for people who have to download it by hand.
    pub page_url: String,
    pub package: Asset,
    pub signature: Asset,
}

pub fn parse_releases(json: &[u8]) -> Result<Vec<GhRelease>, UpdateError> {
    serde_json::from_slice(json).map_err(|e| UpdateError::BadResponse(e.to_string()))
}

/// `v0.5.0-pre-alpha` → 0.5.0. Labels such as `-pre-alpha` don't take part in comparisons.
fn core_version(tag: &str) -> Option<Version> {
    let v = Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()?;
    Some(Version::new(v.major, v.minor, v.patch))
}

pub fn select_update(releases: &[GhRelease], current: &Version, platform: &str) -> Option<Release> {
    let current = Version::new(current.major, current.minor, current.patch);
    releases
        .iter()
        .filter(|r| !r.draft)
        .filter_map(|r| {
            let version = core_version(&r.tag_name)?;
            if version <= current {
                return None;
            }
            let package_name = format!("peeroxide-{version}-{platform}.zip");
            let signature_name = format!("{package_name}.minisig");
            let find = |name: &str| {
                r.assets.iter().find(|a| a.name == name).map(|a| Asset {
                    name: a.name.clone(),
                    size: a.size,
                    url: a.browser_download_url.clone(),
                })
            };
            Some(Release {
                package: find(&package_name)?,
                signature: find(&signature_name)?,
                version,
                tag: r.tag_name.clone(),
                page_url: r.html_url.clone(),
            })
        })
        .max_by(|a, b| a.version.cmp(&b.version))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, draft: bool, assets: &[&str]) -> String {
        let assets: Vec<String> = assets
            .iter()
            .map(|n| {
                format!(r#"{{"name":"{n}","size":123,"browser_download_url":"https://example.com/{n}"}}"#)
            })
            .collect();
        format!(
            r#"{{"tag_name":"{tag}","draft":{draft},"prerelease":true,"html_url":"https://example.com/{tag}","assets":[{}]}}"#,
            assets.join(",")
        )
    }

    fn signed(version: &str) -> [String; 2] {
        let zip = format!("peeroxide-{version}-windows-x64.zip");
        [zip.clone(), format!("{zip}.minisig")]
    }

    fn pick(releases: &[String], current: &str) -> Option<Release> {
        let json = format!("[{}]", releases.join(","));
        let list = parse_releases(json.as_bytes()).unwrap();
        select_update(&list, &Version::parse(current).unwrap(), "windows-x64")
    }

    fn with(tag: &str, draft: bool, assets: &[String]) -> String {
        let names: Vec<&str> = assets.iter().map(String::as_str).collect();
        release(tag, draft, &names)
    }

    #[test]
    fn picks_the_newest_newer_signed_release_including_pre_releases() {
        let releases = [
            with("v0.6.0-pre-alpha", false, &signed("0.6.0")),
            with("v0.5.1-pre-alpha", false, &signed("0.5.1")),
            with("v0.4.0-pre-alpha", false, &signed("0.4.0")),
        ];
        let r = pick(&releases, "0.4.0").unwrap();
        assert_eq!(r.version, Version::new(0, 6, 0));
        assert_eq!(r.tag, "v0.6.0-pre-alpha");
        assert_eq!(r.package.name, "peeroxide-0.6.0-windows-x64.zip");
        assert_eq!(r.signature.name, "peeroxide-0.6.0-windows-x64.zip.minisig");
        assert_eq!(
            r.package.url,
            "https://example.com/peeroxide-0.6.0-windows-x64.zip"
        );
        assert_eq!(r.page_url, "https://example.com/v0.6.0-pre-alpha");
    }

    #[test]
    fn never_the_same_or_an_older_version() {
        let releases = [
            with("v0.4.0-pre-alpha", false, &signed("0.4.0")),
            with("v0.3.0-pre-alpha", false, &signed("0.3.0")),
        ];
        assert_eq!(pick(&releases, "0.4.0"), None);
        // A label on the running version doesn't make the same release look newer.
        assert_eq!(pick(&releases, "0.4.0-pre-alpha"), None);
        assert_eq!(pick(&releases, "0.5.0"), None);
    }

    #[test]
    fn drafts_unsigned_and_other_platforms_are_ignored() {
        let unsigned = ["peeroxide-0.9.0-windows-x64.zip".to_string()];
        let other = [
            "peeroxide-0.8.0-macos-arm64.zip".to_string(),
            "peeroxide-0.8.0-macos-arm64.zip.minisig".to_string(),
        ];
        let releases = [
            with("v1.0.0", true, &signed("1.0.0")),
            with("v0.9.0", false, &unsigned),
            with("v0.8.0", false, &other),
            with("v0.5.0", false, &signed("0.5.0")),
        ];
        assert_eq!(
            pick(&releases, "0.4.0").unwrap().version,
            Version::new(0, 5, 0)
        );
    }

    #[test]
    fn a_package_must_carry_its_own_version() {
        // An old package re-attached to a new tag is not offered as the new version.
        let releases = [with("v0.7.0", false, &signed("0.4.0"))];
        assert_eq!(pick(&releases, "0.4.0"), None);
    }

    #[test]
    fn odd_tags_are_skipped_and_garbage_is_an_error() {
        let releases = [
            with("nightly", false, &signed("0.9.0")),
            with("v0.5.0", false, &signed("0.5.0")),
        ];
        assert_eq!(
            pick(&releases, "0.4.0").unwrap().version,
            Version::new(0, 5, 0)
        );
        assert!(parse_releases(b"<html>rate limited</html>").is_err());
        assert!(parse_releases(b"{\"message\":\"Not Found\"}").is_err());
        assert_eq!(parse_releases(b"[]").unwrap().len(), 0);
    }
}
