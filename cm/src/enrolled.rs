//! Keys this controller has agreed to remember.
//!
//! A laptop is not ephemeral. Making it prove itself with a fresh token on every request
//! would mean a browser round trip before every test run, which nobody would tolerate for
//! long — so it proves itself **once** and leaves a key behind.
//!
//! After that there is **no session**. No token, no expiry, no refresh, nothing to rotate
//! and nothing to leak: a later request is authenticated by the connection it arrives on,
//! because dialling from a key is possession of it. Revocation is this file forgetting the
//! key, which is the only kind of revocation that is instant and cannot be replayed around.
//!
//! ## Why this file is written by the controller and the others are not
//!
//! `admins.toml` and `issuers.toml` are hand-written, because they say who is trusted.
//! This one is a **consequence** of a trust decision already made: somebody proved who they
//! were to an issuer the host had already named, and this records the key they proved it
//! from. Nobody appears here who could not already have asked.
//!
//! Which is also why enrolment is off unless an issuer says `enrol = true`. A CI token
//! proves a *repository*, and a repository is not a machine — letting build tokens enrol
//! would put one permanent key per project in here, which is the thing attestation exists
//! to avoid.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const FILE: &str = "enrolled.toml";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Book {
    #[serde(default, rename = "key")]
    pub keys: Vec<Enrolled>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Enrolled {
    /// The id52 that will be dialling. The whole of the credential.
    pub key: String,
    /// Who they are, as the issuer proved it: `okta:dana@acme.com`. The same shape a
    /// ticket's alias has, so tenancy and policy cannot tell the difference.
    pub alias: String,
    /// Which issuer vouched, and when. For an operator reading this later and wondering.
    pub issuer: String,
    pub at: u64,
    /// What the caller called the machine. Advisory, and never trusted for anything: it is
    /// the one field here somebody could have made up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug)]
pub struct Keys {
    path: PathBuf,
    book: Book,
}

impl Keys {
    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::path_in(root);
        let book: Book = if path.exists() {
            toml::from_str(&std::fs::read_to_string(&path)?)
                .with_context(|| format!("reading {}", path.display()))?
        } else {
            Book::default()
        };
        Ok(Self { path, book })
    }

    pub fn path_in(root: &Path) -> PathBuf {
        root.join(FILE)
    }

    pub fn len(&self) -> usize {
        self.book.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.book.keys.is_empty()
    }

    /// Who this key belongs to, if anybody.
    pub fn who(&self, key: &str) -> Option<&Enrolled> {
        self.book.keys.iter().find(|e| e.key == key)
    }

    /// Remember a key, or update the one already there.
    ///
    /// Re-enrolling the same key is not an error: somebody running `cm auth login` twice
    /// has done nothing wrong, and refusing would leave them with a working key and a
    /// failure message.
    pub fn remember(&mut self, entry: Enrolled) -> Result<()> {
        if let Some(existing) = self.book.keys.iter_mut().find(|e| e.key == entry.key) {
            *existing = entry;
        } else {
            self.book.keys.push(entry);
        }
        self.save()
    }

    /// Forget a key. Returns what was forgotten, so a caller can say whose it was.
    ///
    /// By key rather than by alias: one person may have three machines, and revoking a
    /// stolen laptop should not lock them out of the other two.
    pub fn forget(&mut self, key: &str) -> Result<Option<Enrolled>> {
        let Some(at) = self.book.keys.iter().position(|e| e.key == key) else {
            return Ok(None);
        };
        let gone = self.book.keys.remove(at);
        self.save()?;
        Ok(Some(gone))
    }

    /// Every key one alias holds, for showing somebody what they have.
    pub fn held_by(&self, alias: &str) -> Vec<&Enrolled> {
        self.book.keys.iter().filter(|e| e.alias == alias).collect()
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Written whole and replaced, so a crash mid-write cannot leave a half-parsed file
        // that locks every enrolled machine out at once.
        let text = toml::to_string_pretty(&self.book)?;
        let staging = self.path.with_extension("writing");
        std::fs::write(&staging, text)
            .with_context(|| format!("writing {}", staging.display()))?;
        std::fs::rename(&staging, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, alias: &str) -> Enrolled {
        Enrolled {
            key: key.into(),
            alias: alias.into(),
            issuer: "okta".into(),
            at: 1_787_000_000,
            note: None,
        }
    }

    #[test]
    fn a_missing_file_means_nobody() {
        let root = crate::testing::scratch("enrolled-none");
        let keys = Keys::load(&root).unwrap();
        assert!(keys.is_empty());
        assert!(keys.who("anything").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_key_survives_a_reload() {
        // The whole point of the file: no session, so the key has to still be there
        // tomorrow.
        let root = crate::testing::scratch("enrolled-round");
        let mut keys = Keys::load(&root).unwrap();
        keys.remember(entry("k1", "okta:dana@acme.com")).unwrap();

        let again = Keys::load(&root).unwrap();
        assert_eq!(again.who("k1").unwrap().alias, "okta:dana@acme.com");
        assert_eq!(again.who("k1").unwrap().issuer, "okta");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn enrolling_twice_updates_rather_than_duplicating() {
        // Somebody who runs `cm auth login` twice has done nothing wrong, and two rows for
        // one key would make revoking it a question of which row.
        let root = crate::testing::scratch("enrolled-twice");
        let mut keys = Keys::load(&root).unwrap();
        keys.remember(entry("k1", "okta:dana@acme.com")).unwrap();
        keys.remember(entry("k1", "okta:dana@acme.com")).unwrap();
        assert_eq!(keys.len(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn forgetting_is_by_key_so_one_machine_can_be_revoked() {
        // One person, three laptops. Losing one should not lock them out of the others.
        let root = crate::testing::scratch("enrolled-forget");
        let mut keys = Keys::load(&root).unwrap();
        for k in ["laptop", "desktop", "the-one-on-the-train"] {
            keys.remember(entry(k, "okta:dana@acme.com")).unwrap();
        }
        assert_eq!(keys.held_by("okta:dana@acme.com").len(), 3);

        let gone = keys.forget("the-one-on-the-train").unwrap().unwrap();
        assert_eq!(gone.alias, "okta:dana@acme.com");
        assert!(keys.who("the-one-on-the-train").is_none(), "revocation is immediate");
        assert!(keys.who("laptop").is_some(), "and does not touch the others");

        // Forgetting what was never there is not an error: the outcome asked for is the
        // outcome that holds.
        assert!(keys.forget("never-existed").unwrap().is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_file_is_replaced_rather_than_edited_in_place() {
        // A crash mid-write that left a half-parsed file would lock out every enrolled
        // machine at once, which is a bad way to find out about a full disk.
        let root = crate::testing::scratch("enrolled-atomic");
        let mut keys = Keys::load(&root).unwrap();
        keys.remember(entry("k1", "okta:dana@acme.com")).unwrap();
        assert!(!Keys::path_in(&root).with_extension("writing").exists(), "staging left behind");
        assert!(toml::from_str::<Book>(
            &std::fs::read_to_string(Keys::path_in(&root)).unwrap()
        )
        .is_ok());
        std::fs::remove_dir_all(&root).ok();
    }
}
