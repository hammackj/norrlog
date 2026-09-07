//! Talking to eqdb: the credential, and the one function that sends anything.
//!
//! Two things reach the site from this daemon — a screenshot and its reading,
//! and a contribution bundle — and both do it with the same token, the same
//! base URL and the same rule about what a refusal means. That was one
//! `post` living privately inside the screenshot reader, which is where the
//! second caller would have had to copy it from.
//!
//! # Why the refusal is passed through rather than summarised
//!
//! The site's own words name the reason: a quota, a bundle already held, a
//! 403 on the contributor name. "Upload failed" throws away the only part of
//! the answer worth reading, and this daemon cannot fix any of them — the
//! person in front of it can.

use std::path::Path;

/// Where the token may come from, in order.
///
/// Never a flag. `/proc/<pid>/cmdline` is world-readable and the environment
/// is not, so a token passed as an argument is a token every process on the
/// machine can read — the same rule the eqldb binaries enforce by refusing a
/// database URL on `--db`.
pub fn token_from(config: Option<&Path>) -> Option<String> {
    if let Ok(t) = std::env::var("EQLDB_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    let path = config?.join("eqldb-token");
    let meta = std::fs::metadata(&path).ok()?;
    // A token any account on this machine can read is not a secret, and
    // quietly using it would teach that it was one.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            eprintln!(
                "[eqlparserd] {} is readable by other accounts (mode {:o}); \
                 chmod 600 it. Not using it.",
                path.display(),
                meta.permissions().mode() & 0o7777
            );
            return None;
        }
    }
    let _ = meta;
    let t = std::fs::read_to_string(&path).ok()?.trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// The token this machine holds, wherever it is kept.
pub fn token() -> Option<String> {
    token_from(crate::paths::config_dir().as_deref())
}

/// What a token is missing, said the same way wherever it is missing.
pub const NO_TOKEN: &str = "no eqdb token: set EQLDB_TOKEN, or put one in \
                            ~/.config/eqlparser/eqldb-token with mode 600";

/// An origin and the credential to speak to it with.
pub struct Site {
    http: reqwest::Client,
    base: String,
    token: String,
}

impl Site {
    /// Read the token from wherever it is kept.
    ///
    /// Read per call rather than held, so a token pasted into Settings takes
    /// effect on the next send instead of on the next restart. The screenshot
    /// reader is the other way round — it holds one and is rebuilt when the
    /// token changes — because it is a long-running loop and this is not.
    pub fn new(base: impl Into<String>) -> Result<Site, String> {
        Site::with_token(base, token().ok_or(NO_TOKEN)?)
    }

    pub fn with_token(base: impl Into<String>, token: String) -> Result<Site, String> {
        Ok(Site {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .map_err(|e| e.to_string())?,
            base: base.into(),
            token,
        })
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub async fn post(
        &self,
        path: &str,
        body: reqwest::Body,
        ctype: &str,
        extra: &[(&str, &str)],
    ) -> Result<serde_json::Value, String> {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base.trim_end_matches('/')))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", ctype);
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        let r = req.body(body).send().await.map_err(|e| e.to_string())?;
        let status = r.status();
        let text = r.text().await.unwrap_or_default();
        if !status.is_success() {
            // The site's own words. See the module note.
            return Err(format!(
                "{} {}",
                status.as_u16(),
                serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v
                        .get("detail")
                        .or_else(|| v.get("status"))
                        .and_then(|d| d.as_str().map(str::to_string)))
                    .unwrap_or_else(|| text.chars().take(160).collect())
            ));
        }
        Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
    }
}

/// Gzip, because a bundle is JSON and compresses about ten to one.
///
/// `POST /api/bundle` sniffs the magic bytes rather than reading
/// `Content-Encoding`, and counts the *expanded* size against the ceiling, so
/// compressing is a courtesy to the wire and changes nothing about what
/// arrives. The id the site files it under is the digest of the expanded
/// bytes for the same reason: how a bundle travelled is not part of what it
/// is.
pub fn gzip(json: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(json).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}
