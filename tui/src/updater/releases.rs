//! Official, bounded HTTPS release downloads. Tests substitute an in-memory source.
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::io::Read;
use std::time::Duration;

pub(super) const RELEASES: &str = "https://github.com/humanunsupervised/rakumux/releases/download";
const LATEST: &str = "https://api.github.com/repos/humanunsupervised/rakumux/releases/latest";

pub(super) trait ReleaseSource {
    fn fetch(&self, url: &str, limit: usize) -> Result<Vec<u8>>;
}

pub(super) struct GithubReleases(reqwest::blocking::Client);

impl GithubReleases {
    pub fn new() -> Result<Self> {
        Ok(Self(
            reqwest::blocking::Client::builder()
                .user_agent(concat!("rakumux/", env!("CARGO_PKG_VERSION")))
                .https_only(true)
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(120))
                .build()?,
        ))
    }
}

impl ReleaseSource for GithubReleases {
    fn fetch(&self, url: &str, limit: usize) -> Result<Vec<u8>> {
        let response = self
            .0
            .get(url)
            .send()
            .context("cannot reach GitHub releases")?
            .error_for_status()
            .context("GitHub release download failed")?;
        if response
            .content_length()
            .is_some_and(|length| length > limit as u64)
        {
            bail!("release response exceeds its size limit");
        }
        let mut bytes = Vec::new();
        response.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            bail!("release response exceeds its size limit");
        }
        Ok(bytes)
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

pub(super) fn latest(source: &dyn ReleaseSource) -> Result<String> {
    let release: Release = serde_json::from_slice(&source.fetch(LATEST, 1_048_576)?)?;
    let version = stable_version(&release.tag_name)?;
    if release.draft || release.prerelease {
        bail!("GitHub did not return a stable public release");
    }
    Ok(version.to_string())
}

pub(super) fn stable_version(value: &str) -> Result<semver::Version> {
    let version = semver::Version::parse(value.strip_prefix('v').unwrap_or(value))
        .context("release has an invalid version")?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        bail!("automatic updates require a stable release");
    }
    Ok(version)
}
