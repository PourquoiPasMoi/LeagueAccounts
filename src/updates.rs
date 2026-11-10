//! New-version check against the GitHub releases of the app's repository, and
//! download and integrity check of the release executable.
//!
//! Releases are trusted because only the repository's maintainer can publish
//! them; executables come only from its release assets over HTTPS, and must
//! match the SHA-256 digest GitHub computed when they were uploaded.

use crate::logging::Reason;
use semver::Version;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

/// `owner/name` of the repository whose releases are checked.
/// Our fork: updates come only from its releases, never from upstream.
pub const REPOSITORY: &str = "PourquoiPasMoi/LeagueAccounts";
/// The release asset that replaces the running executable.
const EXECUTABLE_ASSET: &str = "LeagueAccounts.exe";
const MAX_EXECUTABLE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub version: String,
    pub name: String,
    pub url: String,
    pub notes: String,
    pub published_at: String,
    /// The executable, when the release carries one. Not sent to the UI.
    #[serde(skip)]
    pub executable: Option<Download>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Download {
    pub url: String,
    /// Lowercase hex SHA-256 from the asset's `digest`.
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug)]
pub enum UpdateError {
    Request(reqwest::Error),
    Io(io::Error),
    /// Not the release, asset or size that was announced.
    Response,
    /// The download does not match the release's SHA-256 digest.
    Checksum,
}

impl UpdateError {
    /// The log category; never the error text, which can contain URLs or paths.
    pub fn reason(&self) -> Reason {
        match self {
            Self::Request(error) => Reason::from_request(error),
            Self::Io(error) => Reason::from_error(error),
            Self::Response => Reason::ResponseBody,
            Self::Checksum => Reason::Checksum,
        }
    }
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
            Self::Response => f.write_str("unexpected release download"),
            Self::Checksum => f.write_str("the download does not match the release"),
        }
    }
}

impl From<reqwest::Error> for UpdateError {
    fn from(error: reqwest::Error) -> Self {
        Self::Request(error)
    }
}

impl From<io::Error> for UpdateError {
    fn from(error: io::Error) -> Self {
        // Body reads wrap the request error (a timeout, a reset) in io::Error.
        if !error.get_ref().is_some_and(|inner| inner.is::<reqwest::Error>()) {
            return Self::Io(error);
        }
        match error.into_inner().map(|inner| inner.downcast::<reqwest::Error>()) {
            Some(Ok(inner)) => Self::Request(*inner),
            _ => Self::Response,
        }
    }
}

/// Parse a release version, keeping the optional `v` prefix GitHub tags use.
/// Only complete semantic versions are accepted: short forms and unknown
/// segments are not filled in with zeroes.
fn parse_version(text: &str) -> Option<Version> {
    Version::parse(without_v(text)).ok()
}

fn without_v(text: &str) -> &str {
    text.strip_prefix('v')
        .or_else(|| text.strip_prefix('V'))
        .unwrap_or(text)
}

/// Whether `candidate` is a later semantic version than `current`. A leading
/// `v` is accepted; anything that is not a complete semantic version is not
/// newer.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => {
            candidate.cmp_precedence(&current) == Ordering::Greater
        }
        _ => false,
    }
}

/// Only release pages of `REPOSITORY` may be opened from update notices.
pub fn is_release_url(url: &str) -> bool {
    url.starts_with(&format!("https://github.com/{REPOSITORY}/releases/"))
}

/// Executables are downloaded only from `REPOSITORY`'s release assets.
fn is_asset_url(url: &str) -> bool {
    url.starts_with(&format!("https://github.com/{REPOSITORY}/releases/download/"))
}

/// Development builds never replace their own executable.
pub fn can_install() -> bool {
    !cfg!(debug_assertions)
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, UpdateError> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(concat!("LeagueAccounts/", env!("CARGO_PKG_VERSION")))
        .timeout(timeout)
        .build()?)
}

/// The latest published (non-draft, non-prerelease) release, if any.
pub fn latest_release() -> Result<Option<Release>, UpdateError> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let response = client(Duration::from_secs(15))?
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()?;
    // 404: the repository has no published release yet.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body = response.error_for_status()?.text()?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|_| UpdateError::Response)?;
    Ok(parse_release(&json))
}

fn parse_release(json: &serde_json::Value) -> Option<Release> {
    let tag = json["tag_name"].as_str().unwrap_or_default();
    let url = json["html_url"].as_str().unwrap_or_default();
    if parse_version(tag).is_none() || !is_release_url(url) {
        return None;
    }
    Some(Release {
        version: without_v(tag).to_owned(),
        name: json["name"].as_str().unwrap_or(tag).to_owned(),
        url: url.to_owned(),
        notes: json["body"].as_str().unwrap_or_default().chars().take(2000).collect(),
        published_at: json["published_at"].as_str().unwrap_or_default().to_owned(),
        executable: executable_download(json),
    })
}

fn executable_download(json: &serde_json::Value) -> Option<Download> {
    let executable = json["assets"].as_array()?.iter().find(|asset| {
        asset["name"].as_str() == Some(EXECUTABLE_ASSET)
            && asset["state"].as_str().is_none_or(|state| state == "uploaded")
    })?;
    let url = executable["browser_download_url"].as_str().filter(|url| is_asset_url(url))?;
    let size = executable["size"]
        .as_u64()
        .filter(|size| (1..=MAX_EXECUTABLE_BYTES).contains(size))?;
    let sha256 = executable["digest"]
        .as_str()?
        .strip_prefix("sha256:")
        .filter(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))?
        .to_ascii_lowercase();
    Some(Download {
        url: url.to_owned(),
        sha256,
        size,
    })
}

/// Stream the executable into `file`, reporting the bytes received so far.
pub fn download(download: &Download, file: &mut File, mut progress: impl FnMut(u64)) -> Result<(), UpdateError> {
    // Per read, not for the whole transfer: slow connections still finish.
    let mut response = client(Duration::from_secs(30))?
        .get(&download.url)
        .send()?
        .error_for_status()?;
    if response.content_length().is_some_and(|length| length != download.size) {
        return Err(UpdateError::Response);
    }
    let mut buffer = vec![0; 64 * 1024];
    let mut received = 0;
    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        received += read as u64;
        if received > download.size {
            return Err(UpdateError::Response);
        }
        file.write_all(&buffer[..read])?;
        progress(received);
    }
    if received != download.size {
        return Err(UpdateError::Response);
    }
    file.sync_all()?;
    Ok(())
}

/// Check the file on disk, the bytes that will be installed, against the
/// release's SHA-256 digest.
pub fn verify_file(path: &Path, sha256: &str) -> Result<(), UpdateError> {
    verify(File::open(path)?, sha256)
}

fn verify(mut data: impl Read, sha256: &str) -> Result<(), UpdateError> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let read = data.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if format!("{:x}", hasher.finalize()) != sha256 {
        return Err(UpdateError::Checksum);
    }
    Ok(())
}
