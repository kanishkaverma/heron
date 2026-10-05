//! OptChat's own OAuth logins (docs/optchat/IMPL.md "Placement"): the store
//! `<home>/auth.json` in pi's shape, refreshed under an flock on
//! `auth.json.lock`, and the two browser sign-in flows.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};
use sha2::Digest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

use crate::{Credentials, Provider, Token};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginMode {
    /// A loopback callback finishes the sign-in; pasting is still accepted.
    Browser,
    /// No loopback could be bound: the user must paste the code.
    PasteCode,
}

pub struct LoginStart {
    pub url: String,
    pub mode: LoginMode,
    pub callback_port: Option<u16>,
    /// A pasted authorization code or full redirect URL.
    pub code: tokio::sync::oneshot::Sender<String>,
    /// Resolves once the entry is written: `Ok(account label)`.
    pub done: tokio::task::JoinHandle<Result<String, String>>,
}

const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const ANTHROPIC_AUTHORIZE: &str = "https://claude.ai/oauth/authorize";
const ANTHROPIC_TOKEN: &str = "https://platform.claude.com/v1/oauth/token";
const ANTHROPIC_PORT: u16 = 53692;
const ANTHROPIC_PATH: &str = "/callback";
const ANTHROPIC_PASTE_REDIRECT: &str = "https://platform.claude.com/oauth/code/callback";
const ANTHROPIC_SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

const OPENAI_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OPENAI_AUTHORIZE: &str = "https://auth.openai.com/oauth/authorize";
const OPENAI_TOKEN: &str = "https://auth.openai.com/oauth/token";
// The client id only accepts this exact loopback.
const OPENAI_PORT: u16 = 1455;
const OPENAI_PATH: &str = "/auth/callback";
const OPENAI_SCOPES: &str = "openid profile email offline_access";
const OPENAI_JWT_CLAIM: &str = "https://api.openai.com/auth";

const LOGIN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Refresh this long before the stored expiry.
const EXPIRY_MARGIN_MS: i64 = 60_000;

pub(crate) fn store_key(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "anthropic",
        Provider::OpenAI => "openai-codex",
    }
}

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "Claude",
        Provider::OpenAI => "ChatGPT",
    }
}

// ---------------------------------------------------------------------------
// Encoding helpers (no base64 crate in the workspace)
// ---------------------------------------------------------------------------

fn base64url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (k, b)| acc | (*b as u32) << (16 - 8 * k));
        for k in 0..=chunk.len() {
            out.push(TABLE[(n >> (18 - 6 * k) & 63) as usize] as char);
        }
    }
    out
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    for chunk in out.chunks_mut(16) {
        let id = uuid::Uuid::new_v4();
        chunk.copy_from_slice(&id.as_bytes()[..chunk.len()]);
    }
    out
}

fn pkce() -> (String, String) {
    let verifier = base64url(&random_bytes::<32>());
    let challenge = base64url(&sha2::Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

/// The JSON payload of a JWT, unverified (only read for claims we display).
fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    serde_json::from_slice(&base64_decode(payload)?).ok()
}

fn openai_account_id(access: &str) -> Option<String> {
    jwt_claims(access)?
        .get(OPENAI_JWT_CLAIM)?
        .get("chatgpt_account_id")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The store: pi's auth.json, guarded by an flock on auth.json.lock
// ---------------------------------------------------------------------------

fn lock_path(store: &Path) -> PathBuf {
    let mut name = store.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    store.with_file_name(name)
}

/// Held for its Drop: closing the handle releases the flock.
struct StoreLock(#[allow(dead_code)] std::fs::File);

async fn lock_store(store: &Path) -> Result<StoreLock, String> {
    let path = lock_path(store);
    tokio::task::spawn_blocking(move || {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        file.lock()
            .map_err(|e| format!("lock {}: {e}", path.display()))?;
        Ok(StoreLock(file))
    })
    .await
    .map_err(|e| e.to_string())?
}

fn read_store(store: &Path) -> Result<serde_json::Map<String, Value>, String> {
    match std::fs::read(store) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| match v {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .ok_or_else(|| format!("{} exists but is not a JSON object", store.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(format!("read {}: {e}", store.display())),
    }
}

/// Replace one entry, keeping every other key, with an atomic rename.
/// Caller holds the store lock.
fn write_entry(store: &Path, key: &str, entry: Value) -> Result<(), String> {
    let mut map = read_store(store)?;
    map.insert(key.to_string(), entry);
    let bytes = serde_json::to_vec_pretty(&Value::Object(map)).map_err(|e| e.to_string())?;
    let dir = store.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".auth.json.{}.tmp", uuid::Uuid::new_v4()));
    let write = || -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, store)?;
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("write {}: {e}", store.display())
    })
}

/// Access tokens a server answered 401 to: the next `token()` refreshes even
/// if the stored expiry says they are still good. Hashes, not tokens.
static REJECTED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

fn token_hash(access: &str) -> String {
    sha2::Sha256::digest(access.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn reject(access: &str) {
    let mut set = REJECTED.lock().unwrap_or_else(|e| e.into_inner());
    set.get_or_insert_with(HashSet::new).insert(token_hash(access));
}

fn rejected(access: &str) -> bool {
    let set = REJECTED.lock().unwrap_or_else(|e| e.into_inner());
    set.as_ref().is_some_and(|s| s.contains(&token_hash(access)))
}

/// One refresh at a time in this process; the flock covers other processes.
static REFRESH: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `FileCredentials::token`: read the entry, refresh it once under the lock
/// when expired (or rejected), write it back.
pub(crate) async fn file_token(store: &Path, provider: Provider) -> Result<Token, String> {
    let signin = || format!("Sign in to {} for OptChat", provider_name(provider));
    let _serial = REFRESH.lock().await;
    let _lock = lock_store(store).await?;
    let map = read_store(store)?;
    let key = store_key(provider);
    let entry = map
        .get(key)
        .filter(|e| e.get("type").and_then(Value::as_str) == Some("oauth"))
        .ok_or_else(signin)?;
    let access = entry.get("access").and_then(Value::as_str).ok_or_else(signin)?;
    let expires = entry.get("expires").and_then(Value::as_i64).unwrap_or(0);
    if expires > now_ms() + EXPIRY_MARGIN_MS && !rejected(access) {
        let account_id = match provider {
            Provider::Anthropic => None,
            Provider::OpenAI => entry
                .get("accountId")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| openai_account_id(access)),
        };
        return Ok(Token {
            access: access.to_string(),
            account_id,
        });
    }
    let refresh = entry
        .get("refresh")
        .and_then(Value::as_str)
        .ok_or_else(signin)?;
    let fresh = refresh_tokens(provider, refresh)
        .await
        .map_err(|e| format!("{} (refreshing the {} login failed: {e})", signin(), provider_name(provider)))?;
    let mut merged = entry.clone();
    if let (Some(target), Value::Object(new)) = (merged.as_object_mut(), fresh.clone()) {
        target.extend(new);
    }
    write_entry(store, key, merged)?;
    Ok(Token {
        access: fresh["access"].as_str().unwrap_or_default().to_string(),
        account_id: fresh
            .get("accountId")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// The refreshed fields of an entry (`access`, `refresh`, `expires`, and
/// `accountId` for ChatGPT).
async fn refresh_tokens(provider: Provider, refresh: &str) -> Result<Value, String> {
    let client = http();
    let response = match provider {
        Provider::Anthropic => client
            .post(ANTHROPIC_TOKEN)
            .header("accept", "application/json")
            .json(&json!({
                "grant_type": "refresh_token",
                "client_id": ANTHROPIC_CLIENT_ID,
                "refresh_token": refresh,
            }))
            .send()
            .await,
        Provider::OpenAI => client
            .post(OPENAI_TOKEN)
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", OPENAI_CLIENT_ID),
            ])
            .send()
            .await,
    }
    .map_err(|e| e.to_string())?;
    entry_from_response(provider, response).await.map(|(entry, _)| entry)
}

/// Turn a token endpoint response into store fields plus a display label.
async fn entry_from_response(
    provider: Provider,
    response: reqwest::Response,
) -> Result<(Value, String), String> {
    let status = response.status();
    let body = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}: {body}"));
    }
    let data: Value = serde_json::from_str(&body).map_err(|e| format!("bad token JSON: {e}"))?;
    let access = data["access_token"].as_str().ok_or("no access_token")?;
    let refresh = data["refresh_token"].as_str().ok_or("no refresh_token")?;
    let expires_in = data["expires_in"].as_i64().ok_or("no expires_in")?;
    Ok(match provider {
        Provider::Anthropic => {
            let label = data
                .pointer("/account/email_address")
                .and_then(Value::as_str)
                .unwrap_or("Claude")
                .to_string();
            (
                json!({
                    "type": "oauth",
                    "access": access,
                    "refresh": refresh,
                    // pi's convention: five minutes early.
                    "expires": now_ms() + expires_in * 1000 - 5 * 60 * 1000,
                }),
                label,
            )
        }
        Provider::OpenAI => {
            let account_id =
                openai_account_id(access).ok_or("the ChatGPT token carries no account id")?;
            let label = data["id_token"]
                .as_str()
                .and_then(jwt_claims)
                .and_then(|c| c.get("email").and_then(Value::as_str).map(str::to_string))
                .or_else(|| {
                    jwt_claims(access)?
                        .pointer("/https:~1~1api.openai.com~1profile/email")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| account_id.clone());
            (
                json!({
                    "type": "oauth",
                    "access": access,
                    "refresh": refresh,
                    "expires": now_ms() + expires_in * 1000,
                    "accountId": account_id,
                }),
                label,
            )
        }
    })
}

/// A request that failed with 401 retries once after a forced refresh.
pub(crate) enum CallError {
    Unauthorized(String),
    Other(String),
}

pub(crate) async fn authorized<T, F, Fut>(
    credentials: &dyn Credentials,
    provider: Provider,
    mut call: F,
) -> Result<T, String>
where
    F: FnMut(Token) -> Fut,
    Fut: Future<Output = Result<T, CallError>>,
{
    let token = credentials.token(provider).await?;
    match call(token.clone()).await {
        Ok(value) => Ok(value),
        Err(CallError::Other(err)) => Err(err),
        Err(CallError::Unauthorized(_)) => {
            reject(&token.access);
            let token = credentials.token(provider).await?;
            match call(token).await {
                Ok(value) => Ok(value),
                Err(CallError::Other(err) | CallError::Unauthorized(err)) => Err(err),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Sign-in flows
// ---------------------------------------------------------------------------

fn query_string(params: &[(&str, &str)]) -> String {
    let mut url = reqwest::Url::parse("http://x/").expect("static url");
    url.query_pairs_mut().extend_pairs(params);
    url.query().unwrap_or_default().to_string()
}

/// A code from the user's paste: a full redirect URL, `code#state`, a query
/// string, or the bare code.
fn parse_pasted(input: &str) -> (Option<String>, Option<String>) {
    let value = input.trim();
    if value.is_empty() {
        return (None, None);
    }
    let from_pairs = |url: &reqwest::Url| {
        let get = |k: &str| {
            url.query_pairs()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.into_owned())
        };
        (get("code"), get("state"))
    };
    if let Ok(url) = reqwest::Url::parse(value)
        && url.has_host()
    {
        return from_pairs(&url);
    }
    if let Some((code, state)) = value.split_once('#') {
        return (Some(code.to_string()), Some(state.to_string()));
    }
    if value.contains("code=")
        && let Ok(url) = reqwest::Url::parse(&format!("http://x/?{}", value.trim_start_matches('?')))
    {
        return from_pairs(&url);
    }
    (Some(value.to_string()), None)
}

/// One browser callback: the code, plus where to send the page to show.
struct Callback {
    code: String,
    page: oneshot::Sender<Result<String, String>>,
}

/// Bind the loopback (IPv4, plus IPv6 when available: browsers may resolve
/// `localhost` to either).
async fn bind_loopback(port: u16) -> Option<Vec<TcpListener>> {
    let v4 = TcpListener::bind(("127.0.0.1", port)).await.ok()?;
    let mut listeners = vec![v4];
    if let Ok(v6) = TcpListener::bind(("::1", port)).await {
        listeners.push(v6);
    }
    Some(listeners)
}

fn page(title: &str, detail: &str) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        "<!doctype html><meta charset=utf-8><title>OptChat</title>\
         <body style=\"font:16px system-ui;margin:4em auto;max-width:36em\">\
         <h2>{}</h2><p>{}</p></body>",
        esc(title),
        esc(detail)
    )
}

async fn respond(stream: &mut tokio::net::TcpStream, status: &str, html: &str) {
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\n\
         cache-control: no-store\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        html.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(html.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Serve the callback route until the flow ends (this task is aborted).
async fn serve(
    listeners: Vec<TcpListener>,
    path: &'static str,
    state: String,
    provider: Provider,
    callbacks: mpsc::Sender<Result<Callback, String>>,
) {
    let (conn_tx, mut conn_rx) = mpsc::channel(8);
    let mut accepts = Vec::new();
    for listener in listeners {
        let conn_tx = conn_tx.clone();
        accepts.push(tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if conn_tx.send(stream).await.is_err() {
                    break;
                }
            }
        }));
    }
    drop(conn_tx);
    let _abort = AbortOnDrop(accepts);
    let name = provider_name(provider);
    while let Some(mut stream) = conn_rx.recv().await {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 2048];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
            match tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk)).await {
                Ok(Ok(n)) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                _ => break,
            }
        }
        let head = String::from_utf8_lossy(&buf);
        let target = head
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("GET "))
            .and_then(|l| l.split(' ').next())
            .unwrap_or("");
        let Ok(url) = reqwest::Url::parse(&format!("http://localhost{target}")) else {
            respond(&mut stream, "400 Bad Request", &page("Bad request", "")).await;
            continue;
        };
        if url.path() != path {
            respond(&mut stream, "404 Not Found", &page("Not found", "This is OptChat's sign-in callback.")).await;
            continue;
        }
        let get = |k: &str| {
            url.query_pairs()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.into_owned())
        };
        if let Some(error) = get("error") {
            let detail = get("error_description").unwrap_or(error);
            respond(&mut stream, "400 Bad Request", &page(&format!("{name} sign-in failed"), &detail)).await;
            let _ = callbacks.send(Err(format!("{name} authorization failed: {detail}"))).await;
            continue;
        }
        if get("state").as_deref() != Some(state.as_str()) {
            respond(&mut stream, "400 Bad Request", &page("State mismatch", "Start the sign-in again.")).await;
            continue;
        }
        let Some(code) = get("code") else {
            respond(&mut stream, "400 Bad Request", &page("Missing authorization code", "")).await;
            continue;
        };
        let (page_tx, page_rx) = oneshot::channel();
        if callbacks.send(Ok(Callback { code, page: page_tx })).await.is_err() {
            respond(&mut stream, "409 Conflict", &page("Already handled", "")).await;
            continue;
        }
        match page_rx.await {
            Ok(Ok(label)) => {
                respond(
                    &mut stream,
                    "200 OK",
                    &page(&format!("Signed in to {name}"), &format!("{label} — you can close this page and return to OptChat.")),
                )
                .await
            }
            Ok(Err(err)) => respond(&mut stream, "502 Bad Gateway", &page(&format!("{name} sign-in failed"), &err)).await,
            Err(_) => respond(&mut stream, "409 Conflict", &page("Already handled", "")).await,
        }
    }
}

struct AbortOnDrop(Vec<tokio::task::JoinHandle<()>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

struct Flow {
    provider: Provider,
    verifier: String,
    state: String,
    redirect: String,
    store: PathBuf,
}

impl Flow {
    async fn exchange(&self, code: &str, state: &str) -> Result<String, String> {
        let client = http();
        let response = match self.provider {
            Provider::Anthropic => client
                .post(ANTHROPIC_TOKEN)
                .header("accept", "application/json")
                .json(&json!({
                    "grant_type": "authorization_code",
                    "client_id": ANTHROPIC_CLIENT_ID,
                    "code": code,
                    "state": state,
                    "redirect_uri": self.redirect,
                    "code_verifier": self.verifier,
                }))
                .send()
                .await,
            Provider::OpenAI => client
                .post(OPENAI_TOKEN)
                .form(&[
                    ("grant_type", "authorization_code"),
                    ("client_id", OPENAI_CLIENT_ID),
                    ("code", code),
                    ("code_verifier", self.verifier.as_str()),
                    ("redirect_uri", self.redirect.as_str()),
                ])
                .send()
                .await,
        }
        .map_err(|e| format!("token exchange failed: {e}"))?;
        let (entry, label) = entry_from_response(self.provider, response)
            .await
            .map_err(|e| format!("token exchange failed: {e}"))?;
        let _lock = lock_store(&self.store).await?;
        write_entry(&self.store, store_key(self.provider), entry)?;
        Ok(label)
    }

    async fn run(
        self,
        mut callbacks: Option<mpsc::Receiver<Result<Callback, String>>>,
        mut pasted: oneshot::Receiver<String>,
    ) -> Result<String, String> {
        let mut paste_open = true;
        loop {
            tokio::select! {
                callback = async { callbacks.as_mut()?.recv().await }, if callbacks.is_some() => {
                    match callback {
                        Some(Ok(Callback { code, page })) => {
                            let state = self.state.clone();
                            let result = self.exchange(&code, &state).await;
                            let _ = page.send(result.clone());
                            return result;
                        }
                        Some(Err(err)) => return Err(err),
                        None => callbacks = None,
                    }
                }
                input = &mut pasted, if paste_open => {
                    let Ok(input) = input else {
                        paste_open = false;
                        if callbacks.is_none() {
                            return Err("Sign-in cancelled".into());
                        }
                        continue;
                    };
                    let (code, state) = parse_pasted(&input);
                    if state.as_deref().is_some_and(|s| s != self.state) {
                        return Err("The pasted code is from another sign-in (state mismatch)".into());
                    }
                    let code = code.ok_or("The pasted text holds no authorization code")?;
                    let state = state.unwrap_or_else(|| self.state.clone());
                    return self.exchange(&code, &state).await;
                }
            }
            if callbacks.is_none() && !paste_open {
                return Err("Sign-in cancelled".into());
            }
        }
    }
}

/// Start a browser sign-in. The loopback listener is bound before this
/// returns; `done` exchanges the code (from the loopback or a paste) and
/// writes the entry under the store lock.
pub async fn start_login(provider: Provider, store: PathBuf) -> Result<LoginStart, String> {
    let (verifier, challenge) = pkce();
    let (port, path) = match provider {
        Provider::Anthropic => (ANTHROPIC_PORT, ANTHROPIC_PATH),
        Provider::OpenAI => (OPENAI_PORT, OPENAI_PATH),
    };
    let listeners = bind_loopback(port).await;
    let mode = if listeners.is_some() {
        LoginMode::Browser
    } else {
        LoginMode::PasteCode
    };
    let loopback = format!("http://localhost:{port}{path}");
    let (state, redirect, url) = match provider {
        Provider::Anthropic => {
            // Anthropic's flow uses the verifier as the state.
            let state = verifier.clone();
            let redirect = match mode {
                LoginMode::Browser => loopback,
                LoginMode::PasteCode => ANTHROPIC_PASTE_REDIRECT.to_string(),
            };
            let query = query_string(&[
                ("code", "true"),
                ("client_id", ANTHROPIC_CLIENT_ID),
                ("response_type", "code"),
                ("redirect_uri", &redirect),
                ("scope", ANTHROPIC_SCOPES),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("state", &state),
            ]);
            (state, redirect, format!("{ANTHROPIC_AUTHORIZE}?{query}"))
        }
        Provider::OpenAI => {
            let state: String = random_bytes::<16>().iter().map(|b| format!("{b:02x}")).collect();
            // Without the loopback the user pastes the redirect URL the
            // browser failed to open; the redirect must still match.
            let query = query_string(&[
                ("response_type", "code"),
                ("client_id", OPENAI_CLIENT_ID),
                ("redirect_uri", &loopback),
                ("scope", OPENAI_SCOPES),
                ("code_challenge", &challenge),
                ("code_challenge_method", "S256"),
                ("state", &state),
                ("id_token_add_organizations", "true"),
                ("codex_cli_simplified_flow", "true"),
                ("originator", "zeron"),
            ]);
            (state, loopback, format!("{OPENAI_AUTHORIZE}?{query}"))
        }
    };
    let (code_tx, code_rx) = oneshot::channel();
    let flow = Flow {
        provider,
        verifier,
        state: state.clone(),
        redirect,
        store,
    };
    let done = tokio::spawn(async move {
        let (cb_tx, cb_rx) = mpsc::channel(1);
        let server = listeners.map(|listeners| {
            AbortOnDrop(vec![tokio::spawn(serve(listeners, path, state, provider, cb_tx))])
        });
        let callbacks = server.as_ref().map(|_| cb_rx);
        let result = tokio::time::timeout(LOGIN_TIMEOUT, flow.run(callbacks, code_rx))
            .await
            .unwrap_or_else(|_| Err(format!("{} sign-in timed out", provider_name(provider))));
        // Let the browser receive its page before the server goes away.
        tokio::time::sleep(Duration::from_millis(300)).await;
        drop(server);
        result
    });
    Ok(LoginStart {
        url,
        mode,
        callback_port: (mode == LoginMode::Browser).then_some(port),
        code: code_tx,
        done,
    })
}
