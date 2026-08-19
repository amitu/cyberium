//! Admins: the devices allowed to look at and change how this controller runs.
//!
//! A third class beside tenants and workers, and a class it must be — because
//! "a device of our own organisation" is far too broad. A worker is one of those,
//! and a machine that offers capacity has no business reading the whole roster,
//! every live reservation, or anybody's budget.
//!
//! So an admin is **paired explicitly**: its key is written down here, by hand, on
//! the controller. Membership is a list the host maintains, not a property anybody
//! can acquire by connecting.
//!
//! ```text
//! <root>/admins.toml
//!   [[admin]]
//!   name = "ops-laptop"
//!   key  = "k9m2ha4t…"
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const FILE: &str = "admins.toml";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Admin {
    /// What to call them in a log. Local to this controller.
    pub name: String,
    /// The device key that must be on the other end of the connection.
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Admins {
    #[serde(rename = "admin", default, skip_serializing_if = "Vec::is_empty")]
    pub list: Vec<Admin>,
}

impl Admins {
    pub fn path_in(root: &Path) -> PathBuf {
        root.join(FILE)
    }

    /// Read the list. A missing file means nobody is an admin, which is the right
    /// default: an admin has to be added deliberately, never acquired by accident.
    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::path_in(root);
        if !path.exists() {
            return Ok(Self::default());
        }
        toml::from_str(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = Self::path_in(root);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Which admin this key belongs to, if any.
    ///
    /// By **key**, never by name: a name is a label we chose for a log, and only the
    /// key was proven by the connection.
    pub fn by_key(&self, key: &str) -> Option<&Admin> {
        self.list.iter().find(|a| a.key == key)
    }

    pub fn add(&mut self, admin: Admin) -> Result<()> {
        if self.list.iter().any(|a| a.key == admin.key) {
            bail!("that key is already an admin");
        }
        if self.list.iter().any(|a| a.name == admin.name) {
            // Names end up in logs and refusals, so two admins sharing one would
            // make an audit trail ambiguous exactly when it matters.
            bail!("an admin called {:?} already exists", admin.name);
        }
        crate::id52_check(&admin.key)?;
        self.list.push(admin);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real keys. An earlier version of this helper edited one character of a
    /// generated id52 to make them differ, which produced strings that are not
    /// public keys at all — and the validator rightly refused them.
    fn key(_n: u8) -> String {
        sirji::id52::encode(&sirji::SecretKey::generate().public())
    }

    #[test]
    fn nobody_is_an_admin_by_default() {
        let root = std::env::temp_dir().join(format!("cm-admin-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let admins = Admins::load(&root).unwrap();
        // The file being absent must not mean "everyone", which is the failure mode
        // this whole class exists to prevent.
        assert!(admins.list.is_empty());
        assert!(admins.by_key(&key(1)).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn membership_is_by_key_not_name() {
        let mut admins = Admins::default();
        let k = key(2);
        admins.add(Admin { name: "ops".into(), key: k.clone(), note: None }).unwrap();
        assert_eq!(admins.by_key(&k).unwrap().name, "ops");
        // A different key claiming the same name is nobody.
        assert!(admins.by_key(&key(3)).is_none());
    }

    #[test]
    fn duplicates_are_refused() {
        let mut admins = Admins::default();
        let k = key(4);
        admins.add(Admin { name: "ops".into(), key: k.clone(), note: None }).unwrap();
        assert!(admins.add(Admin { name: "other".into(), key: k, note: None }).is_err());
        assert!(
            admins.add(Admin { name: "ops".into(), key: key(5), note: None }).is_err(),
            "a repeated name would make the audit trail ambiguous"
        );
    }

    #[test]
    fn a_key_that_is_not_a_key_is_refused() {
        let mut admins = Admins::default();
        assert!(admins.add(Admin { name: "ops".into(), key: "nonsense".into(), note: None }).is_err());
    }

    #[test]
    fn it_round_trips() {
        let root = std::env::temp_dir().join(format!("cm-admin-rt-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut admins = Admins::default();
        let k = key(6);
        admins.add(Admin { name: "ops".into(), key: k.clone(), note: Some("laptop".into()) }).unwrap();
        admins.save(&root).unwrap();
        assert_eq!(Admins::load(&root).unwrap().by_key(&k).unwrap().name, "ops");
        std::fs::remove_dir_all(&root).ok();
    }
}
