//! Contract tests against a fake plugin: real HTTPS with its own certificate
//! authority, recording every request.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use brok_sdk::{Payload, ShardManifest, Topic};
use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::client::Vault;
use crate::mirror::{Report, sync};
use crate::payload::{SyncRequest, Synced};
use crate::settings::Settings;

const KEY: &str = "test-key-123";

#[derive(Default)]
struct State {
    /// Request path (percent encoded) to status and body.
    routes: BTreeMap<String, (u16, String)>,
    /// `METHOD path` of every request.
    seen: Vec<String>,
}

#[derive(Clone)]
struct Plugin {
    url: String,
    ca: String,
    state: Arc<Mutex<State>>,
}

impl Plugin {
    async fn start() -> Self {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, ca_key);
        let key = KeyPair::generate().unwrap();
        let cert = CertificateParams::new(vec!["127.0.0.1".to_owned()]).unwrap().signed_by(&key, &issuer).unwrap();
        let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("https://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let (acceptor, state) = (acceptor.clone(), Arc::clone(&shared));
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(socket).await else { return };
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut buffer).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buffer[..n]),
                        }
                    }
                    let request = String::from_utf8_lossy(&request).to_string();
                    let mut first = request.split_whitespace();
                    let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
                    let authorized = request.contains(&format!("authorization: Bearer {KEY}"));
                    let (status, body) = {
                        let mut state = state.lock().unwrap();
                        state.seen.push(format!("{method} {path}"));
                        if !authorized {
                            (401, String::new())
                        } else {
                            state.routes.get(path).cloned().unwrap_or((404, String::new()))
                        }
                    };
                    if status == 0 {
                        // Never answers: for the timeout test.
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        return;
                    }
                    let response = format!(
                        "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nlocation: https://example.invalid/\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self { url, ca: ca.pem(), state }
    }

    fn route(&self, path: &str, status: u16, body: &str) {
        self.state.lock().unwrap().routes.insert(path.into(), (status, body.into()));
    }

    fn folder(&self, path: &str, files: &[&str]) {
        self.route(path, 200, &serde_json::json!({ "files": files }).to_string());
    }

    fn settings(&self) -> Settings {
        Settings { url: self.url.clone(), timeout_secs: 5, ..Settings::default() }
    }

    async fn sync(&self, settings: &Settings, data: &Path) -> anyhow::Result<Report> {
        let vault = Vault::new(settings, self.ca.as_bytes(), KEY)?;
        sync(&vault, settings, data).await
    }

    fn seen(&self) -> Vec<String> {
        self.state.lock().unwrap().seen.clone()
    }
}

fn read(data: &Path, path: &str) -> Option<String> {
    std::fs::read_to_string(data.join("vault").join(path)).ok()
}

#[tokio::test]
async fn mirrors_notes_follows_changes_and_deletions_with_get_only() {
    let plugin = Plugin::start().await;
    plugin.folder("/vault/", &["Home.md", "Projetos/", "image.png", ".obsidian/"]);
    plugin.folder("/vault/Projetos/", &["Reunião de produto.md"]);
    plugin.route("/vault/Home.md", 200, "home");
    plugin.route("/vault/Projetos/Reuni%C3%A3o%20de%20produto.md", 200, "notes");
    let data = tempfile::tempdir().unwrap();
    let settings = plugin.settings();

    let first = plugin.sync(&settings, data.path()).await.unwrap();
    assert_eq!(first, Report { written: 2, complete: true, ..Report::default() });
    assert_eq!(read(data.path(), "Projetos/Reunião de produto.md").as_deref(), Some("notes"));

    let again = plugin.sync(&settings, data.path()).await.unwrap();
    assert_eq!(again, Report { unchanged: 2, complete: true, ..Report::default() });

    plugin.route("/vault/Home.md", 200, "home, edited");
    plugin.folder("/vault/", &["Home.md"]);
    let changed = plugin.sync(&settings, data.path()).await.unwrap();
    assert_eq!(changed, Report { written: 1, removed: 1, complete: true, ..Report::default() });
    assert!(!data.path().join("vault/Projetos").exists());

    let seen = plugin.seen();
    assert!(seen.iter().all(|request| request.starts_with("GET /vault/")), "{seen:?}");
    assert!(!seen.iter().any(|request| request.contains(".obsidian") || request.contains("image.png")));
}

#[tokio::test]
async fn excluded_hostile_and_oversized_entries_are_never_written() {
    let plugin = Plugin::start().await;
    plugin.folder(
        "/vault/",
        &["ok.md", "OK.md", "../escape.md", "a/b.md", "C:x.md", "Private/", "draft.excalidraw.md", "huge.md"],
    );
    plugin.route("/vault/ok.md", 200, "fine");
    plugin.route("/vault/OK.md", 200, "fine");
    plugin.route("/vault/huge.md", 200, &"x".repeat(100));
    let data = tempfile::tempdir().unwrap();
    let settings =
        Settings { exclude: vec!["Private".into(), "*.excalidraw.md".into()], max_note_bytes: 10, ..plugin.settings() };
    let report = plugin.sync(&settings, data.path()).await.unwrap();
    assert_eq!(report.written, 1, "{report:?}");
    assert_eq!(report.skipped, 5, "three hostile names, one case clash and one large note: {report:?}");
    assert!(!data.path().join("escape.md").exists());
    let seen = plugin.seen();
    assert!(
        !seen.iter().any(|r| r.contains("Private") || r.contains("excalidraw") || r.contains("escape")),
        "{seen:?}"
    );
}

#[tokio::test]
async fn an_incomplete_walk_deletes_nothing() {
    let plugin = Plugin::start().await;
    plugin.folder("/vault/", &["a.md", "b.md", "sub/"]);
    plugin.folder("/vault/sub/", &["c.md"]);
    for path in ["/vault/a.md", "/vault/b.md", "/vault/sub/c.md"] {
        plugin.route(path, 200, "text");
    }
    let data = tempfile::tempdir().unwrap();
    plugin.sync(&plugin.settings(), data.path()).await.unwrap();

    plugin.route("/vault/sub/", 500, "");
    let report = plugin.sync(&plugin.settings(), data.path()).await.unwrap();
    assert!(!report.complete && report.failed == 1 && report.removed == 0);
    assert!(read(data.path(), "sub/c.md").is_some());

    let limited = Settings { max_notes: 1, ..plugin.settings() };
    let report = plugin.sync(&limited, data.path()).await.unwrap();
    assert!(!report.complete && report.removed == 0);
    assert!(read(data.path(), "b.md").is_some());
}

#[tokio::test]
async fn a_wrong_key_stops_at_once_and_never_appears_in_the_error() {
    let plugin = Plugin::start().await;
    plugin.folder("/vault/", &["a.md"]);
    let data = tempfile::tempdir().unwrap();
    let settings = plugin.settings();
    let vault = Vault::new(&settings, plugin.ca.as_bytes(), "wrong-secret").unwrap();
    let error = format!("{:#}", sync(&vault, &settings, data.path()).await.unwrap_err());
    assert!(error.contains("refused the key") && !error.contains("wrong-secret"), "{error}");
    assert_eq!(plugin.seen().len(), 1, "no retry");
}

#[tokio::test]
async fn another_certificate_authority_redirects_and_silence_are_refused() {
    let plugin = Plugin::start().await;
    let other = Plugin::start().await;
    plugin.folder("/vault/", &[]);
    let data = tempfile::tempdir().unwrap();
    let settings = plugin.settings();
    let vault = Vault::new(&settings, other.ca.as_bytes(), KEY).unwrap();
    assert!(sync(&vault, &settings, data.path()).await.is_err());
    assert!(plugin.seen().is_empty(), "the key was never sent");

    plugin.route("/vault/", 302, "");
    let error = format!("{:#}", plugin.sync(&settings, data.path()).await.unwrap_err());
    assert!(error.contains("302"), "{error}");

    plugin.route("/vault/", 0, "");
    let quick = Settings { timeout_secs: 1, ..plugin.settings() };
    assert!(plugin.sync(&quick, data.path()).await.is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn a_symbolic_link_in_the_mirror_is_not_followed() {
    let plugin = Plugin::start().await;
    plugin.folder("/vault/", &["sub/"]);
    plugin.folder("/vault/sub/", &["a.md"]);
    plugin.route("/vault/sub/a.md", 200, "text");
    let (data, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::create_dir_all(data.path().join("vault")).unwrap();
    std::os::unix::fs::symlink(outside.path(), data.path().join("vault/sub")).unwrap();
    let report = plugin.sync(&plugin.settings(), data.path()).await.unwrap();
    assert_eq!((report.written, report.failed), (0, 1));
    assert!(!outside.path().join("a.md").exists());
}

#[test]
fn manifest_matches_the_code() {
    let manifest = ShardManifest::from_json(include_str!("../shard.json")).unwrap();
    assert_eq!(manifest.name, crate::NAME);
    assert_eq!(manifest.version.to_string(), env!("CARGO_PKG_VERSION"));
    let published = Topic::new(Synced::TOPIC).unwrap();
    assert!(manifest.publishes_to.iter().any(|p| p.matches(&published)));
    let subscribed = Topic::new(SyncRequest::TOPIC).unwrap();
    assert!(manifest.subscribes_to.iter().any(|p| p.matches(&subscribed)));
}
