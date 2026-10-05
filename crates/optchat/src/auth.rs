//! OptChat's own OAuth logins (see docs/optchat/IMPL.md "Placement").
//! STUB on the integration branch: the core branch supplies the real flows
//! with exactly this signature.

use std::path::PathBuf;

use crate::Provider;

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

pub async fn start_login(provider: Provider, store: PathBuf) -> Result<LoginStart, String> {
    let _ = store;
    Err(format!("{provider:?} sign-in is not implemented yet"))
}
