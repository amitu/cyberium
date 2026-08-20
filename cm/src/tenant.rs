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
    /// Which of those members may **change the policy**.
    ///
    /// Host-owned, and it has to be: if a tenant could name its own admins, anybody
    /// who could edit the policy could add themselves to it, and the question "who may
    /// change this" would answer itself. Authority over a rule cannot come from the
    /// rule. Security is deterministic; policy is semantic.
    ///
    /// So this is the one thing about a tenant that is *not* up for interpretation —
    /// which is why it sits in `tenant.toml` with the ceiling and the credits, and not
    /// in the folder the tenant writes.
    ///
    /// An admin is also a member: somebody trusted to write the rules is trusted to
    /// run a test under them, and requiring both lists would mostly produce a bug
    /// where one was forgotten.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub admins: Vec<String>,
    /// Credits this tenant may spend per `window`. `None` means the host has set no
    /// budget, which is different from zero — see `Tenant::budget`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credits: Option<u64>,
    /// The rolling window those credits are counted over, in **seconds**. No
    /// calendar and no timezone: a billing limit that needed to know when a team's
    /// day starts would be answering a question it has no business asking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    /// What this deployment knows about the tenant, and the tenant cannot claim.
    ///
    /// Arrives in the prompt as **attested**, beside the caller's identity, so a policy
    /// can turn on it: `plan = "trial"`, `group = "qa-india"`, `gpu = "allowed"`. cm
    /// attaches no meaning to any of it.
    ///
    /// Here rather than in the tenant's own folder because the whole value is that it
    /// cannot be self-asserted. A tenant that could write its own plan would write the
    /// expensive one. It is also the seam a hosted deployment replaces: the same map,
    /// filled from a real directory instead of a file, which is how a group hierarchy or
    /// a feature flag reaches a policy without cm learning what either is.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub facts: std::collections::BTreeMap<String, String>,
    /// Free-form, for whoever has to work out later why this tenant exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Default for Terms {
    fn default() -> Self {
        // Deliberately modest. A ceiling that has to be raised on purpose is better
        // than one nobody noticed was effectively infinite.
        Self {
            ceiling: 10,
            members: Vec::new(),
            // Nobody, not everyone. A tenant whose admins were unset would otherwise
            // hand its own rules to whoever runs tests.
            admins: Vec::new(),
            facts: std::collections::BTreeMap::new(),
            credits: None,
            window: None,
            note: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Tenant {
    /// The tenant's own name — its folder. Not necessarily any caller's alias.
    pub alias: String,
    pub terms: Terms,
    pub policy: Policy,
    /// Everything this tenant has written down, ready to send. Read once here rather
    /// than per request, so a plea does not pay for a directory walk.
    pub rulebook: crate::rulebook::Rulebook,
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
        let mut all: Vec<&str> = if self.terms.members.is_empty() {
            vec![self.alias.as_str()]
        } else {
            self.terms.members.iter().map(String::as_str).collect()
        };
        // An admin is a member. Listing somebody twice to give them both is a rule whose
        // only real effect is the day it is forgotten.
        for admin in &self.terms.admins {
            if !all.contains(&admin.as_str()) {
                all.push(admin);
            }
        }
        all
    }

    /// May this caller change what this tenant has written down?
    ///
    /// Nobody, unless the host said so. An empty list is not "everyone" — that mistake
    /// would hand the policy to whoever runs tests, which is the whole thing this
    /// answers. It is the same default as `admins.toml`: absent means nobody.
    pub fn may_write(&self, alias: &str) -> bool {
        self.terms.admins.iter().any(|a| a == alias)
    }

    /// What this deployment will attest about them.
    pub fn facts(&self) -> std::collections::BTreeMap<String, String> {
        self.terms.facts.clone()
    }

    /// For a refusal that tells somebody who to ask.
    pub fn admins(&self) -> Vec<&str> {
        self.terms.admins.iter().map(String::as_str).collect()
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
    /// Tenants whose files would not read, and why. Their last good copy is still in
    /// force, which is exactly why this has to be visible somewhere.
    unread: BTreeMap<String, String>,
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
        let mut unread = BTreeMap::new();

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
                // One tenant's typo does not stop the others. It does stop *that*
                // tenant: falling back to default terms would put a ceiling and a budget
                // in force that nobody chose, and the host would never know. Their
                // callers get "not a tenant of this controller", which is at least loud.
                match read(&path, &alias) {
                    Ok(tenant) => {
                        loaded.insert(alias.clone(), tenant);
                    }
                    Err(e) => {
                        eprintln!("cannot read tenant {alias}: {e:#}");
                        unread.insert(alias.clone(), format!("{e:#}"));
                    }
                }
            }
        }

        let by_member = index(&loaded)?;
        Ok(Self { unread, dir, loaded, by_member })
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
            // A plea from somebody we have not heard of: the hopeful case, where a
            // tenant was added since startup. A failure here is not this caller's
            // problem, so it is logged and they are told they are unknown.
            if let Err(e) = self.rescan() {
                eprintln!("cannot re-read tenants: {e:#}");
            }
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
            Err(e) => {
                // Kept serving on the last good copy rather than dropped: a typo should
                // not take a paying tenant offline. But an edit that did not apply is
                // invisible to whoever made it, so it is remembered as well as logged —
                // see `Tenants::unread`, which the operator's own tenant view shows. A
                // host believing new limits are in force while the old ones are is worse
                // than either the outage or the typo.
                eprintln!("keeping the last good copy of {name}: {e:#}");
                self.unread.insert(name.clone(), format!("{e:#}"));
            }
        }

        // Re-check: a membership edit may have removed this caller.
        let name = self.by_member.get(caller)?;
        self.loaded.get(name)
    }

    /// Re-read every tenant from disk.
    ///
    /// Returns the error rather than only logging it, because one caller is a plea that
    /// merely hoped for a new tenant and the other has just replaced a policy and needs
    /// to know whether the thing it wrote can be read.
    /// Edits that did not take: tenant name, and why its files would not read.
    ///
    /// Surfaced in the operator's tenant view, because the copy still in force is the
    /// last one that parsed and nothing about the running fleet looks wrong.
    pub fn unread(&self) -> Vec<(&str, &str)> {
        self.unread.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
    }

    pub fn rescan(&mut self) -> Result<()> {
        let fresh = Self::load(
            self.dir
                .parent()
                .unwrap_or_else(|| std::path::Path::new(".")),
        )?;
        let appeared: Vec<&String> = fresh
            .loaded
            .keys()
            .filter(|k| !self.loaded.contains_key(*k))
            .collect();
        if !appeared.is_empty() {
            println!("tenant(s) appeared: {appeared:?}");
        }
        // Merge rather than replace: a tenant that has just been broken by a hand-edit
        // keeps serving on its last good copy, because a typo should not take a paying
        // tenant offline. The `unread` note is how anybody finds out.
        for (name, tenant) in fresh.loaded {
            self.loaded.insert(name, tenant);
        }
        self.unread = fresh.unread;
        self.by_member = index(&self.loaded)?;
        Ok(())
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
        rulebook: crate::rulebook::Rulebook::load(dir)?,
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
    fn a_malformed_terms_file_does_not_become_default_terms() {
        // The failure this prevents: a host mistypes tenant.toml, the tenant keeps
        // running on a ceiling and a budget nobody chose, and nothing looks wrong.
        // Found by a scenario that appended `admins` after a `[facts]` table header, so
        // TOML put it inside — and the tenant quietly ran with no admins at all.
        let root = temp();
        let dir = Tenants::dir_in(&root).join("acme");
        std::fs::create_dir_all(&dir).unwrap();
        Policy::load(&dir).unwrap();
        std::fs::write(dir.join(FILE), "ceiling = 3\n\n[facts]\nadmins = [\"dana\"]\n").unwrap();

        let tenants = Tenants::load(&root).unwrap();
        assert!(tenants.by_name("acme").is_none(), "it loaded on defaults");
        // And the operator can find out why without reading a log.
        let unread = tenants.unread();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].0, "acme");
        assert!(unread[0].1.contains("tenant.toml"), "{:?}", unread[0].1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn one_tenants_typo_does_not_stop_the_others() {
        let root = temp();
        for (name, terms) in [("good", "ceiling = 3\n"), ("bad", "ceiling = [1]\n")] {
            let dir = Tenants::dir_in(&root).join(name);
            std::fs::create_dir_all(&dir).unwrap();
            Policy::load(&dir).unwrap();
            std::fs::write(dir.join(FILE), terms).unwrap();
        }
        let tenants = Tenants::load(&root).unwrap();
        assert!(tenants.by_name("good").is_some());
        assert!(tenants.by_name("bad").is_none());
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
