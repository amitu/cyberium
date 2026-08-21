//! `cyberium` — the parts of `cm` a deployment may need to replace.
//!
//! `cm` is the reference controller: tenants in folders, terms in `tenant.toml`, spend
//! in a ledger file. That is the whole of what an organisation without an identity
//! service needs, and nowhere near enough for one that has one — a company with groups,
//! sub-groups, user ids and feature flags has all of that in a system of its own, and
//! none of it belongs in an open-source allocator.
//!
//! So the protocol, the fleet, the model call and the decision live here, and the pieces
//! that differ per deployment are behind a seam. `cm test` and `cm worker` need no
//! customisation at all: what varies is what a controller *knows*, never how machines are
//! asked for or handed over.
//!
//! Three roles, all sirji **devices**, none holding any identity state:
//!
//! - **`cm controller`** answers to a name at an organisation's sirji. It owns the
//!   whole picture: which machines are here, what they can do, who has them, and
//!   when to take them back.
//! - **`cm worker`** offers capacity. It finds the controller through their shared
//!   parent, registers, and holds the connection — that connection *is* its
//!   availability.
//! - **`cm test`** is a device of the developer's own sirji. It resolves the
//!   controller, pleads, then talks to the granted machines **directly**.
//!
//! The controller allocates; it never carries the work. Workers never speak to each
//! other — they have nothing to say, because everything needing a view of the whole
//! fleet lives in exactly one place.

pub mod admin;
pub mod attest;
pub mod adviser;
pub mod budget;
pub mod fleet;
pub mod policy;
pub mod policytest;
pub mod rulebook;
pub mod proto;
pub mod tenant;
pub mod testing;
pub mod upload;
pub mod controller;
pub mod directory;

use anyhow::{Context, Result};
use sirji::id52;
// `write_all` here is quinn's own inherent method on the stream, not the tokio trait's —
// importing `AsyncWriteExt` for it would be importing nothing.
use tokio::io::{AsyncBufReadExt, BufReader};

// ---------------------------------------------------------------------------
// where a device lives
// ---------------------------------------------------------------------------

pub const HOME_ENV: &str = "CM_HOME";
pub const HOME_DEFAULT: &str = ".cm";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub name: String,
    pub key: String,
    pub parent: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_hints: Vec<String>,
    #[serde(default)]
    pub root: std::path::PathBuf,
}

pub fn home() -> Result<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os(HOME_ENV) {
        return Ok(std::path::PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow::anyhow!("neither CM_HOME nor HOME is set"))?;
    Ok(std::path::PathBuf::from(home).join(HOME_DEFAULT))
}

pub fn config_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join("cm.toml")
}

pub fn load_config(home: &std::path::Path) -> Result<Config> {
    Ok(toml::from_str(&std::fs::read_to_string(config_path(home))?)?)
}

/// Is this text actually an id52? Used wherever a key arrives as a string from a
/// human, so a typo is caught where it was typed rather than at the first dial.
pub fn id52_check(key: &str) -> Result<()> {
    id52::decode(key).map(|_| ()).with_context(|| format!("{key:?} is not an id52"))
}

pub fn keys(home: &std::path::Path) -> sirji::Keystore {
    sirji::Keystore::at(home.join("keys"))
}

/// Where an endpoint is reachable, for the parent to hand on.
pub async fn listening(endpoint: &sirji::Endpoint) -> Vec<String> {
    sirji::endpoint::reachable_at(endpoint).await
}

pub async fn write_line<T: serde::Serialize>(send: &mut sirji::SendStream, value: &T) -> Result<()> {
    send.write_all(format!("{}\n", serde_json::to_string(value)?).as_bytes())
        .await?;
    Ok(())
}

pub async fn read_line<T: serde::de::DeserializeOwned>(
    recv: &mut BufReader<sirji::RecvStream>,
) -> Result<Option<T>> {
    let mut line = String::new();
    if recv.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(line.trim())?))
}

/// How an exit status reads to a person.
pub fn describe(code: Option<i32>) -> String {
    match code {
        Some(0) => "success".to_string(),
        Some(n) => format!("exit {n}"),
        None => "no exit code (killed)".to_string(),
    }
}

pub fn b64_encode(bytes: &[u8]) -> String {
    data_encoding::BASE64.encode(bytes)
}

pub fn b64_decode(text: &str) -> Result<Vec<u8>> {
    Ok(data_encoding::BASE64.decode(text.as_bytes())?)
}

/// Errors that are how the transport reports normal endings, not faults.
///
/// A caller that finished hangs up; iroh races several paths to a machine and
/// abandons the ones that lose. Printing those trains everyone to ignore the log,
/// which is worse than printing nothing.
pub fn quiet(e: &anyhow::Error) -> bool {
    let text = format!("{e:#}");
    text.contains("closed by peer")
        || text.contains("connection closed")
        || text.contains("during the handshake")
}

