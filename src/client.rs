//! The only part that talks to the plugin. It can do nothing but `GET`: the
//! API key could change or delete notes, so no other method exists here.

use anyhow::Context;
use futures_util::StreamExt;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::header::{AUTHORIZATION, ETAG, HeaderValue, IF_NONE_MATCH};
use reqwest::{Certificate, Client, Response, StatusCode, Url};
use serde::Deserialize;

use crate::settings::{API_KEY_ENV, Settings};

/// Characters left as they are in a path segment.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');
/// Largest directory listing accepted.
const MAX_LISTING_BYTES: u64 = 4 * 1024 * 1024;

/// Why a request failed, as far as the sync cares.
#[derive(Debug)]
pub enum Failure {
    /// The key is refused or throttled: stop, retrying only makes it worse.
    Denied(String),
    /// This request failed; others may work.
    Other(anyhow::Error),
}

/// A note as fetched.
#[derive(Debug)]
pub enum Note {
    /// The server says the copy with this tag is current.
    Unchanged,
    /// Larger than the limit; not downloaded in full.
    TooLarge,
    Content {
        bytes: Vec<u8>,
        etag: Option<String>,
    },
}

#[derive(Deserialize)]
struct Listing {
    files: Vec<String>,
}

pub struct Vault {
    http: Client,
    base: Url,
}

impl Vault {
    /// A client that trusts only the plugin's certificate authority, follows no
    /// redirect and uses no proxy, so the key reaches only the configured host.
    pub fn new(settings: &Settings, ca_pem: &[u8], key: &str) -> anyhow::Result<Self> {
        let base = settings.base_url()?;
        let ca = Certificate::from_pem(ca_pem).context("`ca_file` is not a PEM certificate")?;
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", key.trim()))
            .map_err(|_| anyhow::anyhow!("{API_KEY_ENV} holds characters a request cannot carry"))?;
        authorization.set_sensitive(true);
        let http = Client::builder()
            .default_headers([(AUTHORIZATION, authorization)].into_iter().collect())
            .tls_certs_only([ca])
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(settings.timeout())
            .build()
            .context("cannot build the HTTPS client")?;
        Ok(Self { http, base })
    }

    /// The URL of a vault path given as safe parts; `folder` adds the final `/`.
    fn url(&self, parts: &[String], folder: bool) -> anyhow::Result<Url> {
        let mut path = String::from("/vault/");
        path.push_str(&parts.iter().map(|p| utf8_percent_encode(p, SEGMENT).to_string()).collect::<Vec<_>>().join("/"));
        if folder && !parts.is_empty() {
            path.push('/');
        }
        self.base.join(&path).context("cannot build the request address")
    }

    async fn get(&self, url: Url, etag: Option<&str>) -> Result<Response, Failure> {
        let mut request = self.http.get(url);
        if let Some(etag) = etag.and_then(|e| HeaderValue::from_str(e).ok()) {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let response = request.send().await.map_err(|e| {
            // The URL is dropped from the message: it names the user's notes.
            Failure::Other(anyhow::Error::new(e.without_url()).context("cannot reach the Obsidian plugin"))
        })?;
        match response.status() {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err(Failure::Denied(format!("the plugin refused the key in {API_KEY_ENV}")))
            }
            StatusCode::TOO_MANY_REQUESTS => {
                Err(Failure::Denied("the plugin is throttling requests; try again in a minute".into()))
            }
            status if status.is_success() || status == StatusCode::NOT_MODIFIED => Ok(response),
            status => Err(Failure::Other(anyhow::anyhow!("the plugin answered {status}"))),
        }
    }

    /// The entries of a folder; folders end with `/`.
    pub async fn list(&self, parts: &[String]) -> Result<Vec<String>, Failure> {
        let url = self.url(parts, true).map_err(Failure::Other)?;
        let response = self.get(url, None).await?;
        let bytes = read_capped(response, MAX_LISTING_BYTES)
            .await
            .map_err(Failure::Other)?
            .context("a folder listing is too large")
            .map_err(Failure::Other)?;
        let listing: Listing = serde_json::from_slice(&bytes)
            .context("the plugin sent a listing this shard does not understand")
            .map_err(Failure::Other)?;
        Ok(listing.files)
    }

    /// A note's bytes, unless `etag` is still current or it exceeds `max` bytes.
    pub async fn note(&self, parts: &[String], etag: Option<&str>, max: u64) -> Result<Note, Failure> {
        let url = self.url(parts, false).map_err(Failure::Other)?;
        let response = self.get(url, etag).await?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(Note::Unchanged);
        }
        let etag = response.headers().get(ETAG).and_then(|v| v.to_str().ok()).map(str::to_owned);
        Ok(match read_capped(response, max).await.map_err(Failure::Other)? {
            Some(bytes) => Note::Content { bytes, etag },
            None => Note::TooLarge,
        })
    }
}

/// The body, or `None` as soon as it exceeds `max` bytes.
async fn read_capped(response: Response, max: u64) -> anyhow::Result<Option<Vec<u8>>> {
    if response.content_length().is_some_and(|length| length > max) {
        return Ok(None);
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.without_url())?;
        if body.len() as u64 + chunk.len() as u64 > max {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}
