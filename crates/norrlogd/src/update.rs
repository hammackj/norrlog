//! What the app needs from GitHub to update itself: the newest release, and
//! one of its files with its signature.
//!
//! The daemon asks on the app's behalf because it has the TLS stack and the
//! app is built without one. Nothing fetched here is trusted: the app checks
//! each file's minisign signature against the public key it was built with,
//! and installs nothing that fails.

use norrlog_api::{FetchedUpdate, Release, ReleaseAsset};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The project whose releases these are.
const REPO: &str = "hammackj/norrlog";

/// How long a release listing is reused. Launches close together ask once.
const FRESH: Duration = Duration::from_secs(15 * 60);

/// Larger than any build is or should be; a bound on a broken answer.
const MAX_DOWNLOAD: u64 = 1 << 30;
const MAX_SIGNATURE: u64 = 4096;

/// The GitHub API, or another server that answers the same way, for tests.
fn api_base() -> String {
    std::env::var("NORRLOG_UPDATE_API")
        .ok()
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| s.starts_with("https://") || s.starts_with("http://"))
        .unwrap_or_else(|| "https://api.github.com".to_string())
}

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            // GitHub refuses a request with no agent. A generic one: nothing
            // about who runs this goes with it.
            .user_agent(concat!("norrlog-updater/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default()
    })
}

/// A release and where each of its files is.
#[derive(Clone)]
struct Found {
    release: Release,
    urls: HashMap<String, String>,
}

/// The last answer and when it came: `None` inside is "no release".
type Cached = Option<(Instant, Option<Found>)>;

fn cache() -> &'static Mutex<Cached> {
    static CACHE: OnceLock<Mutex<Cached>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

#[derive(serde::Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(serde::Deserialize)]
struct GhAsset {
    name: String,
    #[serde(default)]
    size: u64,
    browser_download_url: String,
}

/// The newest published release, or `None` when there is none. GitHub's
/// "latest" leaves out drafts and pre-releases, and a private repository
/// answers 404 to anyone without a token, which also reads as none.
async fn latest_found(fresh: bool) -> Result<Option<Found>, String> {
    if !fresh {
        if let Some((at, found)) = cache().lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            if at.elapsed() < FRESH {
                return Ok(found.clone());
            }
        }
    }
    let url = format!("{}/repos/{REPO}/releases/latest", api_base());
    let resp = client()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| format!("could not reach GitHub: {e}"))?;
    let found = if resp.status() == reqwest::StatusCode::NOT_FOUND {
        None
    } else if !resp.status().is_success() {
        return Err(format!("GitHub answered {}", resp.status()));
    } else {
        let gh: GhRelease = resp
            .json()
            .await
            .map_err(|e| format!("an unreadable release listing: {e}"))?;
        Some(Found {
            urls: gh
                .assets
                .iter()
                .map(|a| (a.name.clone(), a.browser_download_url.clone()))
                .collect(),
            release: Release {
                name: gh.name.clone().unwrap_or_else(|| gh.tag_name.clone()),
                tag: gh.tag_name,
                notes: gh.body.unwrap_or_default(),
                url: gh.html_url,
                assets: gh
                    .assets
                    .into_iter()
                    .map(|a| ReleaseAsset {
                        name: a.name,
                        size: a.size,
                    })
                    .collect(),
            },
        })
    };
    *cache().lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), found.clone()));
    Ok(found)
}

pub async fn latest() -> Result<Option<Release>, String> {
    Ok(latest_found(false).await?.map(|f| f.release))
}

/// A file name that stays a file name: the asset names come from the
/// release listing, and are used to name files here.
fn plain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}

/// Download `asset` of release `tag` and its `.minisig` into the data
/// directory's `updates/<tag>/`.
pub async fn fetch(tag: &str, asset: &str, into: &Path) -> Result<FetchedUpdate, String> {
    if !plain(tag) || !plain(asset) {
        return Err("not a release file name".into());
    }
    let mut found = latest_found(false).await?;
    if found.as_ref().is_none_or(|f| f.release.tag != tag) {
        found = latest_found(true).await?;
    }
    let found = found
        .filter(|f| f.release.tag == tag)
        .ok_or_else(|| format!("{tag} is not the newest release"))?;
    let sig_name = format!("{asset}.minisig");
    let (Some(url), Some(sig_url)) = (found.urls.get(asset), found.urls.get(&sig_name)) else {
        return Err(format!("{tag} has no signed {asset}"));
    };
    let dir = into.join(tag);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let signature = download(sig_url, &dir.join(&sig_name), MAX_SIGNATURE).await?;
    let signature = std::fs::read_to_string(&signature).map_err(|e| e.to_string())?;
    let path = download(url, &dir.join(asset), MAX_DOWNLOAD).await?;
    Ok(FetchedUpdate {
        path: path.display().to_string(),
        signature,
    })
}

/// Stream `url` into `to`, through a `.part` file so a broken download is
/// never taken for a whole one.
async fn download(url: &str, to: &Path, limit: u64) -> Result<PathBuf, String> {
    use std::io::Write;
    let mut resp = client()
        .get(url)
        .timeout(Duration::from_secs(30 * 60))
        .send()
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: {}", resp.status()));
    }
    let part = to.with_extension("part");
    let mut file = std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut got: u64 = 0;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("download failed: {e}"))?
    {
        got += chunk.len() as u64;
        if got > limit {
            let _ = std::fs::remove_file(&part);
            return Err("the download is larger than any release".into());
        }
        file.write_all(&chunk).map_err(|e| e.to_string())?;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&part, to).map_err(|e| e.to_string())?;
    Ok(to.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_names_name_files() {
        assert!(plain("norrlog-0.2.0-x86_64.AppImage"));
        assert!(plain("v0.2.0"));
        for bad in ["", "../x", "a/b", "a\\b", ".hidden", "a b", "é"] {
            assert!(!plain(bad), "{bad}");
        }
    }
}
