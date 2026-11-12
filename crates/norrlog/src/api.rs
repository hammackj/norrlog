//! Requests from a view: `Client::get`, run on the daemon thread's tokio
//! runtime (reqwest needs one) and awaited from GPUI's executor.

use norrlog_client::Client;
use serde::de::DeserializeOwned;
use std::future::Future;

#[derive(Clone)]
pub struct Api {
    pub rt: tokio::runtime::Handle,
    pub client: Client,
}

impl Api {
    pub fn get<T: DeserializeOwned + Send + 'static>(&self, path: String) -> impl Future<Output = Result<T, String>> + use<T> {
        let client = self.client.clone();
        let task = self.rt.spawn(async move { client.get::<T>(&path).await.map_err(|e| e.to_string()) });
        async move { task.await.map_err(|e| e.to_string())? }
    }

    /// A GET's raw body, for an image.
    pub fn get_bytes(&self, path: String) -> impl Future<Output = Result<Vec<u8>, String>> + use<> {
        let client = self.client.clone();
        let task = self.rt.spawn(async move { client.get_bytes(&path).await.map_err(|e| e.to_string()) });
        async move { task.await.map_err(|e| e.to_string())? }
    }

    /// A POST of a JSON body; only whether it worked comes back.
    pub fn post_json(&self, path: String, body: serde_json::Value) -> impl Future<Output = Result<(), String>> + use<> {
        let client = self.client.clone();
        // A refusal reads as the daemon's own words, as the renderer shows
        // the response body.
        let task = self.rt.spawn(async move {
            client.post_json(&path, &body).await.map_err(|e| match e {
                norrlog_client::Error::Api { error, .. } => error,
                e => e.to_string(),
            })
        });
        async move { task.await.map_err(|e| e.to_string())? }
    }

    /// The daemon's origin, for a URL handed to the browser.
    pub fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.client.port())
    }
}

/// `application/x-www-form-urlencoded`, as `URLSearchParams` writes it.
pub fn query(pairs: &[(&str, String)]) -> String {
    let enc = |s: &str| {
        let mut out = String::new();
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => out.push(b as char),
                b' ' => out.push('+'),
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    };
    pairs.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

#[cfg(test)]
mod tests {
    #[test]
    fn query_encodes_as_urlsearchparams_does() {
        let q = super::query(&[("search", "a priest of najena".into()), ("build", "SHD/BRD".into()), ("limit", "300".into())]);
        assert_eq!(q, "search=a+priest+of+najena&build=SHD%2FBRD&limit=300");
    }
}
