//! One folder per tenant, and how the controller picks which one a caller belongs to.
//!
//! **The tenant key is the verified alias from the caller's ticket**, and that is the
//! whole trick. The alias is minted by *our own* sirji from its `network.toml`, so it
//! is not something the caller asserts — it is our record of who they are. Which
//! means multi-tenancy needs no new credential and no attestation machinery: it works
//! with what the controller already verifies today.
//!
//! ```text
//! <root>/tenants/
//!   acme/
//!     tenant.toml      what the host decides about them — chiefly a ceiling
//!     policy.md        what they decide for themselves
//!   beta/
//!     tenant.toml
//!     policy.md
//! ```
//!
//! The split between those two files is the point: **a tenant writes `policy.md`, the
//! host writes `tenant.toml`.** Without that, an organisation authoring its own policy
//! would be authoring its own quota, and `standing_limit: 10000` is a valid file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::policy::Policy;

pub const DIR: &str = "tenants";
pub const FILE: &str = "tenant.toml";

/// What the host says about a tenant. Never written by the tenant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Terms {
    /// The most machines this tenant may hold at once, whatever their own policy
    /// says. The outer clamp — in a hosted deployment this is what they bought.
    pub ceiling: u32,
    /// Free-form, for whoever has to work out later why this tenant exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Default for Terms {
    fn default() -> Self {
        // Deliberately modest. A ceiling that has to be raised on purpose is better
        // than one nobody noticed was effectively infinite.
        Self { ceiling: 10, note: None }
    }
}

#[derive(Debug, Clone)]
pub struct Tenant {
    pub alias: String,
    pub terms: Terms,
    pub policy: Policy,
}

impl Tenant {
    /// How many machines this tenant may have, given what their policy allowed.
    ///
    /// Returns the number and, when it was cut, why — so a caller reading "you get
    /// 10" is told it was the ceiling rather than left to guess at their own policy.
    pub fn clamp(&self, allowed: u32) -> (u32, Option<String>) {
        if allowed <= self.terms.ceiling {
            (allowed, None)
        } else {
            (
                self.terms.ceiling,
                Some(format!(
                    "policy allowed {allowed}, capped at this tenant's ceiling of {}",
                    self.terms.ceiling
                )),
            )
        }
    }
}

/// Every tenant this controller serves.
#[derive(Debug)]
pub struct Tenants {
    dir: PathBuf,
    loaded: BTreeMap<String, Tenant>,
}

impl Tenants {
    pub fn dir_in(root: &Path) -> PathBuf {
        root.join(DIR)
    }

    /// Read and **validate every tenant** at startup.
    ///
    /// All of them, not lazily: a policy that does not parse should stop the
    /// controller starting, not surface an hour later on somebody's first plea with
    /// them waiting on the other end.
    pub fn load(root: &Path) -> Result<Self> {
        let dir = Self::dir_in(root);
        let mut loaded = BTreeMap::new();

        if dir.is_dir() {
            for entry in std::fs::read_dir(&dir)
                .with_context(|| format!("reading {}", dir.display()))?
            {
                let path = entry?.path();
                if !path.is_dir() {
                    continue;
                }
                let alias = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(|| anyhow::anyhow!("{} is not a usable name", path.display()))?
                    .to_string();
                loaded.insert(alias.clone(), read(&path, &alias)?);
            }
        }

        Ok(Self { dir, loaded })
    }

    pub fn aliases(&self) -> impl Iterator<Item = &String> {
        self.loaded.keys()
    }

    pub fn len(&self) -> usize {
        self.loaded.len()
    }

    /// The tenant a caller belongs to, re-read from disk if it has changed.
    ///
    /// Re-reading means `cm tenant add` and a policy edit take effect without a
    /// restart. A re-read that fails keeps the last known-good copy and complains,
    /// because a typo in a file should not take an organisation offline mid-run.
    pub fn get(&mut self, alias: &str) -> Option<&Tenant> {
        if !self.loaded.contains_key(alias) && self.dir.join(alias).is_dir() {
            // Added since startup.
            match read(&self.dir.join(alias), alias) {
                Ok(tenant) => {
                    println!("tenant {alias} appeared");
                    self.loaded.insert(alias.to_string(), tenant);
                }
                Err(e) => eprintln!("tenant {alias} is unusable: {e:#}"),
            }
        } else if self.loaded.contains_key(alias) {
            match read(&self.dir.join(alias), alias) {
                Ok(fresh) => {
                    self.loaded.insert(alias.to_string(), fresh);
                }
                Err(e) => eprintln!("keeping the last good policy for {alias}: {e:#}"),
            }
        }
        self.loaded.get(alias)
    }

    /// Create a tenant, with a starter policy they can edit.
    ///
    /// Refuses to overwrite: an existing folder holds somebody's rules, and quietly
    /// replacing them with a starter file would be the worst possible outcome of a
    /// mistyped command.
    pub fn add(root: &Path, alias: &str, terms: Terms) -> Result<PathBuf> {
        if !alias
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            // It becomes a directory name, so it does not get to contain surprises.
            bail!("{alias:?} should be lowercase letters, digits, - and _");
        }

        let dir = Self::dir_in(root).join(alias);
        if dir.exists() {
            bail!("{} already exists", dir.display());
        }
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join(FILE), toml::to_string_pretty(&terms)?)?;
        // Writes the starter policy as a side effect, which is what we want here:
        // a tenant that exists but has no policy would refuse every request.
        Policy::load(&dir)?;
        Ok(dir)
    }
}

fn read(dir: &Path, alias: &str) -> Result<Tenant> {
    let terms_path = dir.join(FILE);
    let terms: Terms = if terms_path.exists() {
        toml::from_str(&std::fs::read_to_string(&terms_path)?)
            .with_context(|| format!("parsing {}", terms_path.display()))?
    } else {
        // A tenant folder with no terms gets the default ceiling rather than an
        // unlimited one. Missing host configuration must never mean "no limit".
        Terms::default()
    };

    Ok(Tenant {
        alias: alias.to_string(),
        terms,
        policy: Policy::load(dir)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cm-tenant-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn adding_a_tenant_gives_them_something_to_edit() {
        let root = temp();
        let dir = Tenants::add(&root, "acme", Terms::default()).unwrap();
        assert!(dir.join("tenant.toml").exists());
        assert!(dir.join("policy.md").exists(), "a starter policy, or every plea refuses");

        let tenants = Tenants::load(&root).unwrap();
        assert_eq!(tenants.aliases().collect::<Vec<_>>(), vec!["acme"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_tenant_is_never_silently_replaced() {
        let root = temp();
        Tenants::add(&root, "acme", Terms::default()).unwrap();
        let err = Tenants::add(&root, "acme", Terms::default()).unwrap_err();
        assert!(format!("{err:#}").contains("already exists"), "{err:#}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_name_that_would_surprise_a_filesystem_is_refused() {
        let root = temp();
        for bad in ["../escape", "Acme", "a name", "acme/x"] {
            assert!(Tenants::add(&root, bad, Terms::default()).is_err(), "{bad:?}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_ceiling_cuts_what_policy_allowed() {
        let root = temp();
        Tenants::add(&root, "acme", Terms { ceiling: 4, note: None }).unwrap();
        let mut tenants = Tenants::load(&root).unwrap();
        let tenant = tenants.get("acme").unwrap();

        assert_eq!(tenant.clamp(3), (3, None), "under the ceiling, untouched");
        let (count, why) = tenant.clamp(50);
        assert_eq!(count, 4);
        // The caller is told which limit bit them, since it is not their own.
        assert!(why.unwrap().contains("ceiling of 4"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_terms_are_modest_not_unlimited() {
        let root = temp();
        let dir = Tenants::dir_in(&root).join("acme");
        std::fs::create_dir_all(&dir).unwrap();
        Policy::load(&dir).unwrap();

        let mut tenants = Tenants::load(&root).unwrap();
        let tenant = tenants.get("acme").unwrap();
        assert_eq!(tenant.terms.ceiling, Terms::default().ceiling);
        assert_eq!(tenant.clamp(9999).0, Terms::default().ceiling);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_unknown_caller_has_no_tenant() {
        let root = temp();
        Tenants::add(&root, "acme", Terms::default()).unwrap();
        let mut tenants = Tenants::load(&root).unwrap();
        assert!(tenants.get("somebody-else").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_tenant_added_later_is_picked_up_without_a_restart() {
        let root = temp();
        let mut tenants = Tenants::load(&root).unwrap();
        assert_eq!(tenants.len(), 0);

        Tenants::add(&root, "late", Terms::default()).unwrap();
        assert!(tenants.get("late").is_some(), "no restart should be needed");
        std::fs::remove_dir_all(&root).ok();
    }
}
