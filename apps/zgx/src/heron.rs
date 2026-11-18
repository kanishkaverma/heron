//! Heron.app: this interface as a window onto the one Heron chat (the
//! `heron` feature; ui/screens/heron says what the window shows).
//!
//! Heron is not a Zeron of its own. The account, the chats and the engine are
//! Zeron.app's, and Zeron.app may be open beside it. So a Heron build:
//!
//! - attaches to the engine Zeron runs and never starts one (it has none in
//!   its bundle: src/engine/manager.rs);
//! - never asks that engine to stop, update, sign out or delete the account
//!   ([`refuses`], checked where every call leaves: src/engine/rpc.rs);
//! - never updates itself: the updater would put Zeron.app in its place
//!   (src/about.rs);
//! - takes no `zeron://` links and no banner clicks, runs no plugins or
//!   templates, and registers no Appshot shortcut (src/main.rs);
//! - keeps what it remembers (its window, its scripts' state) under its own
//!   name, apart from Zeron's (`APP_ID` in src/main.rs).

/// Whether this build is Heron.app.
pub const ON: bool = cfg!(feature = "heron");

/// The engine's calls a Heron build never makes: each changes Zeron for
/// every window and device, not this chat.
pub fn refuses(method: &str) -> bool {
    ON && matches!(method, "StopEngine" | "ApplyUpdate" | "SignOut" | "DeleteAccount")
}

#[cfg(test)]
mod tests {
    #[test]
    fn refuses_only_in_a_heron_build() {
        for method in ["StopEngine", "ApplyUpdate", "SignOut", "DeleteAccount"] {
            assert_eq!(super::refuses(method), super::ON, "{method}");
        }
        assert!(!super::refuses("EngineInfo"));
        assert!(!super::refuses("WatchChats"));
    }
}
