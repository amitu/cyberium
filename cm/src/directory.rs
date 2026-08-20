//! The seam: what a controller knows about the people asking it for machines.
//!
//! `cm` keeps that in files — a folder per tenant, terms in `tenant.toml`, spend in a
//! ledger. For an organisation without an identity service that is the whole answer. For
//! one that has groups, sub-groups, user ids and a feature-flag system, it is the wrong
//! answer in every particular, and none of that shape belongs in an open-source
//! allocator.
//!
//! So it is a trait, and [`Folders`] is one implementation of it. A deployment writes
//! another against its own directory and builds its own binary around
//! [`crate::controller`]. Nothing else changes: `cm test` and `cm worker` are untouched,
//! because what varies is what a controller *knows*, never how machines are asked for or
//! handed over.
//!
//! ## What is deliberately not here
//!
//! **The policy.** A [`Tenancy`] carries the tenant's rules as *text*, not as parsed
//! rules, because the decision is a model reading everything they wrote — see
//! [`crate::rulebook`]. A deployment storing policy in a database returns the same string
//! from a different place, and the decision does not know the difference.
//!
//! **The flags.** There is no `is_enabled(feature)`. An access hierarchy and a
//! feature-flag system arrive as [`Tenancy::facts`], get attested in the prompt, and are
//! read by the policy — so `group`, `sub_group`, `plan` and `flag:gpu` are pairs cm
//! carries and never interprets. A gate here would be cm growing an opinion about a
//! vocabulary that is not its own.
//!
//! **Authority over the rules.** `may_write` is a `bool` the deployment decides, and it
//! must not come from the policy: if a policy named its own admins, anybody who could
//! edit it could add themselves. Authority over a rule cannot come from the rule.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;

use crate::{adviser, budget, proto, tenant, upload};

/// Everything a controller needs to decide one caller's plea.
///
/// Assembled per request, so a deployment that wants to charge for a plan change halfway
/// through a day simply answers differently next time.
#[derive(Debug, Clone)]
pub struct Tenancy {
    /// Who pays, and whose rules apply. Not the caller: several callers share a tenant.
    pub tenant: String,
    /// May this caller ask for machines at all?
    ///
    /// A directory question, not a policy one — `cm` answers it from `requesters:` in the
    /// fenced block, a company answers it from whether the user exists and is active.
    pub may_ask: bool,
    /// May this caller change the rules? See the module note: never from the rules.
    pub may_write: bool,
    /// What this deployment will attest about them: team, plan, group, entitlements.
    /// Carried into the prompt, never interpreted here.
    pub facts: BTreeMap<String, String>,
    /// Everything they have written down, as text. A file tree and its contents for
    /// `cm`; whatever renders the same for anybody else.
    pub rulebook: String,
    /// What this organisation calls an ordinary request. Calibration for the model.
    pub standing: u32,
    /// The most any answer may be, after every cap that applies has been taken into
    /// account. Enforced whatever the model says.
    pub ceiling: u32,
    /// How long a grant survives unreleased, in seconds.
    pub lifetime: u64,
    /// What they may spend, if anything is metered.
    pub budget: Option<Budget>,
}

#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub credits: u64,
    /// Rolling, in seconds. No calendar and no timezone — a named calendar is a thing a
    /// policy expresses in prose.
    pub window: u64,
}

/// One tenant as an operator sees it, for `cm admin spend`.
#[derive(Debug, Clone)]
pub struct Listed {
    pub tenant: String,
    pub budget: Option<Budget>,
    /// Set when this tenant's own configuration would not read, and the copy in force is
    /// therefore not the one on disk. Invisible from every other angle, so it is carried
    /// here rather than only logged.
    pub unread: Option<String>,
}

/// What a decision was about, for [`Directory::reviewed`].
#[derive(Debug, Clone, Copy)]
pub struct Weighing<'a> {
    /// The caller, as their ticket proved them.
    pub caller: &'a str,
    /// Everything this deployment said about them.
    pub tenancy: &'a Tenancy,
    /// How many machines they asked for.
    pub asked: u32,
    /// The keys they sent. cm read none of them, and neither did the policy's author
    /// unless they wrote a rule about one.
    pub said: &'a BTreeMap<String, String>,
}

#[async_trait]
pub trait Directory: Send + Sync {
    /// Who is this caller, and what do they get? `None` if this deployment has never
    /// heard of them, which is a different answer from "not allowed" and said differently.
    async fn look_up(&self, caller: &str) -> Result<Option<Tenancy>>;

    /// Credits spent in the last `window` seconds.
    async fn spent(&self, tenant: &str, window: u64) -> Result<u64>;

    /// Record what a finished reservation cost. Called after the machines are already
    /// back, so a failure here loses money rather than capacity — worth logging loudly
    /// and never worth refusing over.
    async fn charge(&self, tenant: &str, entry: &budget::Entry) -> Result<()>;

    /// Replace what a tenant has written down. Authority is checked before this is
    /// called; validating the contents is this implementation's business.
    async fn write_rules(&self, tenant: &str, up: &proto::Upload) -> Result<Vec<String>>;

    /// Every tenant, for the operator's own view. Never for a caller.
    async fn roster(&self) -> Result<Vec<Listed>>;

    /// The model's answer, before any of it is enforced. Change it, or do not.
    ///
    /// This is where a deployment's own business logic goes, and it is a `&mut` rather
    /// than a return value because most implementations will only want to *read*: emit a
    /// metric, write a decision to an audit log, count how often a policy is overshooting.
    ///
    /// It can also decide. An account in arrears, a region that is over capacity, an
    /// incident freeze — the sort of rule that belongs to whoever runs the fleet rather
    /// than to any one tenant's policy, and that nobody wants to express in prose.
    ///
    /// **Every clamp still applies afterwards.** Raising the count here does not escape
    /// the ceiling, the budget or what is free; lowering it always works. So this is a
    /// place to be stricter, or to watch, and not a way around the numbers a human wrote
    /// — which is the same rule the model itself is held to.
    ///
    /// An `Err` fails the request rather than being ignored. If your audit log is the
    /// reason you are allowed to hand out machines, a decision you could not record is a
    /// decision you should not act on; if it is not, log and return `Ok`.
    ///
    /// Not called by `cm policy-test`, which runs against a folder with no deployment
    /// behind it. A policy test therefore checks the policy, not this — worth knowing if
    /// you put a rule here that a tenant could otherwise read about in their own files.
    async fn reviewed(
        &self,
        _about: &Weighing<'_>,
        _opinion: &mut adviser::Opinion,
    ) -> Result<()> {
        Ok(())
    }

    /// A line for the startup log, so an operator can see what is answering.
    fn describe(&self) -> String;
}

// ---------------------------------------------------------------------------
// tenants in folders — the reference implementation
// ---------------------------------------------------------------------------

/// Tenants as directories under `<root>/tenants/`.
///
/// Re-reads on lookup, so adding a tenant or editing a policy takes effect without a
/// restart. Behind a lock for that reason and no other.
pub struct Folders {
    tenants: tokio::sync::Mutex<tenant::Tenants>,
    root: PathBuf,
}

impl Folders {
    pub fn load(root: &std::path::Path) -> Result<Self> {
        Ok(Self {
            tenants: tokio::sync::Mutex::new(tenant::Tenants::load(root)?),
            root: root.to_path_buf(),
        })
    }

    fn dir_of(&self, tenant: &str) -> PathBuf {
        tenant::Tenants::dir_in(&self.root).join(tenant)
    }

    /// How many tenants are onboarded, for the startup line.
    pub async fn count(&self) -> usize {
        self.tenants.lock().await.len()
    }

    pub async fn names(&self) -> Vec<String> {
        self.tenants.lock().await.names().cloned().collect()
    }
}

#[async_trait]
impl Directory for Folders {
    async fn look_up(&self, caller: &str) -> Result<Option<Tenancy>> {
        let mut tenants = self.tenants.lock().await;
        let Some(tenant) = tenants.for_caller(caller) else {
            return Ok(None);
        };
        let (standing, own_ceiling) = tenant.policy.bounds();
        Ok(Some(Tenancy {
            tenant: tenant.alias.clone(),
            may_ask: tenant.policy.may_ask(caller),
            may_write: tenant.may_write(caller),
            facts: tenant.facts(),
            rulebook: tenant.rulebook.as_str().to_string(),
            standing,
            // The tightest that applies. Springing the host's cap after the fact would
            // have a model argue for a number it was never allowed to give.
            ceiling: own_ceiling.min(tenant.ceiling()),
            lifetime: tenant.policy.reservation_secs(),
            budget: tenant.budget().map(|(credits, window)| Budget { credits, window }),
        }))
    }

    async fn spent(&self, tenant: &str, window: u64) -> Result<u64> {
        Ok(budget::spent(&self.dir_of(tenant), crate::fleet::now(), window))
    }

    async fn charge(&self, tenant: &str, entry: &budget::Entry) -> Result<()> {
        budget::record(&self.dir_of(tenant), entry)
    }

    async fn write_rules(&self, tenant: &str, up: &proto::Upload) -> Result<Vec<String>> {
        let written = upload::accept(&self.dir_of(tenant), up)?;
        // Re-read at once: a controller still weighing pleas against the folder it just
        // replaced would be applying a policy that no longer exists.
        self.tenants.lock().await.rescan()?;
        Ok(written)
    }

    async fn roster(&self) -> Result<Vec<Listed>> {
        let tenants = self.tenants.lock().await;
        let unread: BTreeMap<String, String> = tenants
            .unread()
            .into_iter()
            .map(|(n, w)| (n.to_string(), w.to_string()))
            .collect();
        let mut out: Vec<Listed> = tenants
            .all()
            .map(|t| Listed {
                tenant: t.alias.clone(),
                budget: t.budget().map(|(credits, window)| Budget { credits, window }),
                unread: unread.get(&t.alias).cloned(),
            })
            .collect();
        // Tenants whose files would not read at all are not in `all()`, and they are the
        // ones an operator most needs to see.
        for (name, why) in unread {
            if !out.iter().any(|l| l.tenant == name) {
                out.push(Listed { tenant: name, budget: None, unread: Some(why) });
            }
        }
        out.sort_by(|a, b| a.tenant.cmp(&b.tenant));
        Ok(out)
    }

    fn describe(&self) -> String {
        format!("tenants in {}", tenant::Tenants::dir_in(&self.root).display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy;

    async fn folders(terms: &str, policy_text: &str) -> (PathBuf, Folders) {
        let root = crate::testing::scratch("directory");
        let dir = tenant::Tenants::dir_in(&root).join("acme");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(tenant::FILE), terms).unwrap();
        std::fs::write(dir.join(crate::policy::FILE), policy_text).unwrap();
        let folders = Folders::load(&root).unwrap();
        (root, folders)
    }

    #[tokio::test]
    async fn a_lookup_carries_everything_one_decision_needs() {
        let (root, dir) = folders(
            "ceiling = 3\nmembers = [\"dana\"]\nadmins = [\"dana\"]\ncredits = 60\nwindow = 3600\n\n[facts]\nplan = \"trial\"\n",
            "```yaml\nstanding_limit: 2\nmax_limit: 9\nreservation_seconds: 60\n```\n\nBe reasonable.\n",
        )
        .await;
        let t = dir.look_up("dana").await.unwrap().unwrap();
        assert_eq!(t.tenant, "acme");
        assert!(t.may_ask && t.may_write);
        assert_eq!(t.standing, 2);
        // The tightest cap that applies, not the tenant's own: the host said three.
        assert_eq!(t.ceiling, 3, "the host's ceiling was not applied");
        assert_eq!(t.lifetime, 60);
        assert_eq!(t.facts.get("plan").map(String::as_str), Some("trial"));
        assert!(t.rulebook.contains("Be reasonable."), "the rules travel as text");
        let b = t.budget.unwrap();
        assert_eq!((b.credits, b.window), (60, 3600));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_caller_nobody_has_heard_of_is_none_not_a_refusal() {
        // Two different answers with two different fixes: one needs onboarding, the other
        // needs permission. Collapsing them sends people to the wrong place.
        let (root, dir) = folders("ceiling = 3\n", "```yaml\nstanding_limit: 2\n```\n").await;
        assert!(dir.look_up("stranger").await.unwrap().is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_member_may_ask_and_may_not_write() {
        let (root, dir) = folders(
            "ceiling = 3\nmembers = [\"dana\", \"kiran\"]\nadmins = [\"dana\"]\n",
            "```yaml\nstanding_limit: 2\n```\n",
        )
        .await;
        let kiran = dir.look_up("kiran").await.unwrap().unwrap();
        assert!(kiran.may_ask, "a member may plead");
        assert!(!kiran.may_write, "a member may not rewrite the rules");
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn the_roster_shows_a_tenant_whose_own_files_will_not_read() {
        // The state that is invisible from every other angle: still serving, on terms that
        // are not the ones in its file.
        let root = crate::testing::scratch("directory-unread");
        let dir = tenant::Tenants::dir_in(&root).join("acme");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(tenant::FILE), "ceiling = [1]\n").unwrap();
        std::fs::write(dir.join(policy::FILE), "```yaml\nstanding_limit: 2\n```\n").unwrap();

        let listed = Folders::load(&root).unwrap().roster().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].tenant, "acme");
        assert!(listed[0].unread.is_some(), "an unreadable tenant vanished from the roster");
        std::fs::remove_dir_all(&root).ok();
    }
}
