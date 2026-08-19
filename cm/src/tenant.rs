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
    /// Which caller aliases belong to this tenant. Empty means just the tenant's
    /// own name, which is the common case.
    ///
    /// Here because a tenant is not always one caller: self-hosted, a tenant is
    /// usually a *team*, and a team has several people. Host-owned for the obvious
    /// reason — a tenant that could name its own members could claim somebody
    /// else's callers, and with them somebody else's budget.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<String>,
    /// Credits this tenant may spend per `window`. `None` means the host has set no
    /// budget, which is different from zero — see `Tenant::budget`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<u64>,
    /// The rolling window those credits are counted over, in **seconds**. No
    /// calendar and no timezone: a billing limit that needed to know when a team's
    /// day starts would be answering a question it has no business asking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    /// Free-form, for whoever has to work out later why this tenant exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Default for Terms {
    fn default() -> Self {
        // Deliberately modest. A ceiling that has to be raised on purpose is better
        // than one nobody noticed was effectively infinite.
        Self { ceiling: 10, members: Vec::new(), credits: None, window: None, note: None }
    }
}

#[derive(Debug, Clone)]
pub struct Tenant {
    /// The tenant's own name — its folder. Not necessarily any caller's alias.
    pub alias: String,
    pub terms: Terms,
    pub policy: Policy,
    /// The pleas this tenant will hear. Empty means it wrote none, and free text is
    /// still accepted from it.
    pub nivedanas: crate::nivedana::Nivedanas,
}

impl Tenant {
    /// The budget in force, as (credits, window seconds), or `None` if neither the
    /// host nor the tenant set one.
    ///
    /// The **lower** of the two when both do, for the same reason as the ceiling: a
    /// tenant dividing what it has must not be able to enlarge it.
    pub fn budget(&self) -> Option<(u64, u64)> {
        let host = self.terms.credits.map(|c| {
            (c, self.terms.window.unwrap_or(crate::budget::WINDOW_SECS))
        });
        let own = self.policy.budget();
        match (host, own) {
            (Some(h), Some(o)) => Some(if o.0 <= h.0 { o } else { h }),
            (Some(h), None) => Some(h),
            (None, Some(o)) => Some(o),
            (None, None) => None,
        }
    }

    /// The caller aliases that count as this tenant.
    pub fn members(&self) -> Vec<&str> {
        if self.terms.members.is_empty() {
            vec![self.alias.as_str()]
        } else {
            self.terms.members.iter().map(String::as_str).collect()
        }
    }
}

impl Tenant {
    /// The host's hard cap on this tenant, whatever their own policy says.
    ///
    /// Shown to the model, and re-checked after it answers. Cutting an over-large
    /// answer down to it lives with the other post-checks, in `sanity`.
    pub fn ceiling(&self) -> u32 {
        self.terms.ceiling
    }
}

/// Every tenant this controller serves.
#[derive(Debug)]
pub struct Tenants {
    dir: PathBuf,
    loaded: BTreeMap<String, Tenant>,
    /// caller alias → tenant name. Several aliases may point at one tenant, which
    /// is what makes a tenant able to be a team.
    by_member: BTreeMap<String, String>,
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

        let by_member = index(&loaded)?;
        Ok(Self { dir, loaded, by_member })
    }

    /// Tenant names — the folders. **Not** caller aliases.
    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.loaded.keys()
    }

    pub fn all(&self) -> impl Iterator<Item = &Tenant> {
        self.loaded.values()
    }

    /// A tenant by its **own name**, for administration.
    ///
    /// Distinct from `for_caller`, and the two are easy to confuse — a tenant's name
    /// need not be any caller's alias, which is the whole point of `members`. Getting
    /// this wrong made `cm tenant add` unable to read back what it had just written.
    pub fn by_name(&self, name: &str) -> Option<&Tenant> {
        self.loaded.get(name)
    }

    pub fn len(&self) -> usize {
        self.loaded.len()
    }

    /// The tenant this **caller alias** belongs to, re-read from disk if it changed.
    ///
    /// Re-reading means `cm tenant add`, a membership change and a policy edit all
    /// take effect without a restart. A re-read that fails keeps the last known-good
    /// copy and complains, because a typo in a file should not take a team offline
    /// mid-run.
    pub fn for_caller(&mut self, caller: &str) -> Option<&Tenant> {
        // An alias we have never seen may belong to a tenant added since startup, or
        // to a membership line edited since. Only a rescan can tell, and it is one
        // readdir on a path taken by unknown callers alone.
        if !self.by_member.contains_key(caller) {
            self.rescan();
        }

        let name = self.by_member.get(caller)?.clone();
        match read(&self.dir.join(&name), &name) {
            Ok(fresh) => {
                self.loaded.insert(name.clone(), fresh);
                // Membership may have moved with that read.
                if let Ok(fresh_index) = index(&self.loaded) {
                    self.by_member = fresh_index;
                }
            }
            Err(e) => eprintln!("keeping the last good policy for {name}: {e:#}"),
        }

        // Re-check: a membership edit may have removed this caller.
        let name = self.by_member.get(caller)?;
        self.loaded.get(name)
    }

    fn rescan(&mut self) {
        match Self::load(
            self.dir
                .parent()
                .unwrap_or_else(|| std::path::Path::new(".")),
        ) {
            Ok(fresh) => {
                let appeared: Vec<&String> = fresh
                    .loaded
                    .keys()
                    .filter(|k| !self.loaded.contains_key(*k))
                    .collect();
                if !appeared.is_empty() {
                    println!("tenant(s) appeared: {appeared:?}");
                }
                self.loaded = fresh.loaded;
                self.by_member = fresh.by_member;
            }
            Err(e) => eprintln!("cannot re-read tenants: {e:#}"),
        }
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

/// caller alias → tenant name, refusing ambiguity.
///
/// Two tenants claiming one caller is not a thing to resolve by picking a winner:
/// whoever's budget it lands against would be arbitrary, and nobody would know which.
fn index(loaded: &BTreeMap<String, Tenant>) -> Result<BTreeMap<String, String>> {
    let mut by_member: BTreeMap<String, String> = BTreeMap::new();
    for tenant in loaded.values() {
        for member in tenant.members() {
            if let Some(other) = by_member.get(member) {
                bail!(
                    "both {other:?} and {:?} claim the caller {member:?} — \
                     whose budget would that spend?",
                    tenant.alias
                );
            }
            by_member.insert(member.to_string(), tenant.alias.clone());
        }
    }
    Ok(by_member)
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
        nivedanas: crate::nivedana::Nivedanas::load(dir)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = crate::testing::scratch("tenant");
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
        assert_eq!(tenants.names().collect::<Vec<_>>(), vec!["acme"]);
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
        Tenants::add(&root, "acme", Terms { ceiling: 4, ..Terms::default() }).unwrap();
        let mut tenants = Tenants::load(&root).unwrap();
        let tenant = tenants.for_caller("acme").unwrap();

        // The host's number, not the tenant's. What cuts an over-large answer down to
        // it lives with the other post-checks in `sanity`, which is tested there.
        assert_eq!(tenant.ceiling(), 4);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_terms_are_modest_not_unlimited() {
        let root = temp();
        let dir = Tenants::dir_in(&root).join("acme");
        std::fs::create_dir_all(&dir).unwrap();
        Policy::load(&dir).unwrap();

        let mut tenants = Tenants::load(&root).unwrap();
        let tenant = tenants.for_caller("acme").unwrap();
        assert_eq!(tenant.terms.ceiling, Terms::default().ceiling);
        assert_eq!(tenant.ceiling(), Terms::default().ceiling);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_tenants_name_is_not_a_caller_lookup() {
        // The two lookups are different, and confusing them made `cm tenant add`
        // unable to read back what it had just written. A team's name is nobody's
        // alias unless somebody listed it.
        let root = temp();
        Tenants::add(
            &root,
            "payments",
            Terms { ceiling: 5, members: vec!["dana".into()], ..Terms::default() },
        )
        .unwrap();
        let mut tenants = Tenants::load(&root).unwrap();

        assert!(tenants.by_name("payments").is_some(), "administration finds it");
        assert!(tenants.for_caller("payments").is_none(), "no caller is called that");
        assert!(tenants.for_caller("dana").is_some(), "its member is");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_tenant_can_be_a_team() {
        // The self-hosted shape: a tenant is a team, and a team has several people.
        let root = temp();
        Tenants::add(
            &root,
            "payments",
            Terms {
                ceiling: 5,
                members: vec!["dana".into(), "kiran".into()],
                ..Terms::default()
            },
        )
        .unwrap();
        let mut tenants = Tenants::load(&root).unwrap();

        assert_eq!(tenants.for_caller("dana").unwrap().alias, "payments");
        assert_eq!(tenants.for_caller("kiran").unwrap().alias, "payments");
        // Both spend the same budget, which is the point of grouping them.
        assert_eq!(tenants.for_caller("kiran").unwrap().terms.ceiling, 5);
        // And the tenant's own name is not a caller unless it was listed.
        assert!(tenants.for_caller("payments").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_members_means_the_tenants_own_name() {
        let root = temp();
        Tenants::add(&root, "acme", Terms::default()).unwrap();
        let mut tenants = Tenants::load(&root).unwrap();
        assert_eq!(tenants.for_caller("acme").unwrap().alias, "acme");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn two_tenants_cannot_claim_one_caller() {
        // Picking a winner would put somebody's spend on somebody else's budget,
        // arbitrarily, and nobody would know which.
        let root = temp();
        Tenants::add(&root, "payments", Terms { ceiling: 5, members: vec!["dana".into()], ..Terms::default() }).unwrap();
        Tenants::add(&root, "platform", Terms { ceiling: 5, members: vec!["dana".into()], ..Terms::default() }).unwrap();

        let err = Tenants::load(&root).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("dana"), "{text}");
        assert!(text.contains("whose budget"), "{text}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_unknown_caller_has_no_tenant() {
        let root = temp();
        Tenants::add(&root, "acme", Terms::default()).unwrap();
        let mut tenants = Tenants::load(&root).unwrap();
        assert!(tenants.for_caller("somebody-else").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_tenant_added_later_is_picked_up_without_a_restart() {
        let root = temp();
        let mut tenants = Tenants::load(&root).unwrap();
        assert_eq!(tenants.len(), 0);

        Tenants::add(&root, "late", Terms::default()).unwrap();
        assert!(tenants.for_caller("late").is_some(), "no restart should be needed");
        std::fs::remove_dir_all(&root).ok();
    }
}
