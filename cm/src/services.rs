//! Services this machine has enrolled with, and the key it uses for each.
//!
//! The client half of [`crate::enrolled`]. One **fresh keypair per service**, which is the
//! same rule the substrate follows for peers: sirji mints a key per relationship so no two
//! peers can correlate you, and a service is no different. Reusing one key everywhere would
//! let two unrelated fleets discover they are talking to the same laptop.
//!
//! What is stored is a public key and a name. The secret lives in the keystore, where every
//! other secret this machine holds already lives — a second place to keep private keys is a
//! second place to get it wrong.
//!
//! There is no token here, and nothing expires. That is the point of enrolling: after it,
//! the connection is the credential.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const FILE: &str = "services.toml";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Book {
    #[serde(default, rename = "service")]
    pub services: Vec<Service>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Service {
    /// How the service is named — the host, as typed. What `cm t` will be given.
    pub at: String,
    /// The key minted for this service, and nowhere else.
    pub key: String,
    /// Who the service said we are. For showing somebody, never for proving anything: the
    /// service decides this, and asks again every time it matters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_whom: Option<String>,
    pub at_time: u64,
}

#[derive(Debug)]
pub struct Services {
    path: PathBuf,
    book: Book,
}

impl Services {
    pub fn load(home: &Path) -> Result<Self> {
        let path = home.join(FILE);
        let book: Book = if path.exists() {
            toml::from_str(&std::fs::read_to_string(&path)?)
                .with_context(|| format!("reading {}", path.display()))?
        } else {
            Book::default()
        };
        Ok(Self { path, book })
    }

    /// The key to dial this service with, if we have enrolled.
    ///
    /// Matched on the name as typed, deliberately: `cm.acme.com` and its id52 are the same
    /// service, but a machine that enrolled by name should keep working when the key
    /// rotates, and one that enrolled by key should not silently follow a name somewhere
    /// else.
    pub fn key_for(&self, at: &str) -> Option<&Service> {
        self.book.services.iter().find(|s| s.at == at)
    }

    pub fn all(&self) -> &[Service] {
        &self.book.services
    }

    pub fn remember(&mut self, service: Service) -> Result<()> {
        match self.book.services.iter_mut().find(|s| s.at == service.at) {
            Some(existing) => *existing = service,
            None => self.book.services.push(service),
        }
        self.save()
    }

    pub fn forget(&mut self, at: &str) -> Result<Option<Service>> {
        let Some(index) = self.book.services.iter().position(|s| s.at == at) else {
            return Ok(None);
        };
        let gone = self.book.services.remove(index);
        self.save()?;
        Ok(Some(gone))
    }

    fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let staging = self.path.with_extension("writing");
        std::fs::write(&staging, toml::to_string_pretty(&self.book)?)
            .with_context(|| format!("writing {}", staging.display()))?;
        std::fs::rename(&staging, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(at: &str, key: &str) -> Service {
        Service {
            at: at.into(),
            key: key.into(),
            as_whom: Some("okta:dana@acme.com".into()),
            at_time: 1_787_000_000,
        }
    }

    #[test]
    fn a_key_per_service_and_no_sharing_between_them() {
        // The substrate mints a key per peer so no two peers can correlate you. A service
        // is no different, and one key everywhere would let two unrelated fleets discover
        // they are talking to the same laptop.
        let home = crate::testing::scratch("services");
        let mut book = Services::load(&home).unwrap();
        book.remember(service("cm.acme.com", "k-acme")).unwrap();
        book.remember(service("cm.other.test", "k-other")).unwrap();

        let again = Services::load(&home).unwrap();
        assert_eq!(again.key_for("cm.acme.com").unwrap().key, "k-acme");
        assert_eq!(again.key_for("cm.other.test").unwrap().key, "k-other");
        assert!(again.key_for("cm.never.test").is_none());
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn logging_in_twice_replaces_rather_than_accumulates() {
        let home = crate::testing::scratch("services-twice");
        let mut book = Services::load(&home).unwrap();
        book.remember(service("cm.acme.com", "old")).unwrap();
        book.remember(service("cm.acme.com", "new")).unwrap();
        assert_eq!(book.all().len(), 1);
        assert_eq!(book.key_for("cm.acme.com").unwrap().key, "new");
        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn logging_out_of_one_service_leaves_the_others() {
        let home = crate::testing::scratch("services-out");
        let mut book = Services::load(&home).unwrap();
        book.remember(service("cm.acme.com", "k-acme")).unwrap();
        book.remember(service("cm.other.test", "k-other")).unwrap();

        assert_eq!(book.forget("cm.acme.com").unwrap().unwrap().key, "k-acme");
        assert!(book.key_for("cm.acme.com").is_none());
        assert!(book.key_for("cm.other.test").is_some());
        assert!(book.forget("cm.never.test").unwrap().is_none());
        std::fs::remove_dir_all(&home).ok();
    }
}
