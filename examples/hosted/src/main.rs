//! A controller whose callers live somewhere other than a folder.
//!
//! This is the shape a company ends up with: users in a directory, arranged in groups and
//! sub-groups, with entitlements decided by a feature-flag system. None of that belongs in
//! `cm`, and `cm` does not need to know any of it — it needs a [`Directory`].
//!
//! Everything else is shared. The protocol, the fleet, the model call, the budget clamp,
//! `cm test` and `cm worker`: unchanged, and unaware that this binary exists.
//!
//! The two things worth copying from here:
//!
//! 1. **A group hierarchy and a feature flag are `facts`, not code.** They go into
//!    `Tenancy::facts`, arrive in the prompt as attested — proven, and unforgeable by the
//!    caller — and the *policy* decides what they earn. There is no `is_enabled()` in the
//!    trait, and adding one would be cm growing an opinion about a vocabulary that is not
//!    its own. Look at `policy.md` below: it reads a plan and a sub-group by name, and
//!    nothing in this file or in cm knows what either means.
//!
//! 2. **`may_write` never comes from the policy.** If a policy named its own admins,
//!    anybody who could edit it could add themselves. Authority over a rule cannot come
//!    from the rule, so it is answered here, from the directory.
//!
//! Run it exactly like `cm controller` — same `cm init`, same `CM_HOME`, same
//! `CM_MODEL_KEY`:
//!
//! ```sh
//! cargo run -p hosted-controller
//! ```

use std::collections::BTreeMap;

use anyhow::Result;
use async_trait::async_trait;
use cyberium::budget;
use cyberium::directory::{Budget, Directory, Listed, Tenancy};
use cyberium::proto;

/// Stands in for what a real deployment would call: an identity service, a plan service,
/// a flag service. Hard-coded here so the example is one file and no infrastructure.
struct Hosted;

/// One row of what the directory would return.
struct Person {
    /// Who pays. A sub-group here, because that is the unit a bill goes to.
    tenant: &'static str,
    group: &'static str,
    sub_group: &'static str,
    user_id: &'static str,
    plan: &'static str,
    /// Flags this person's sub-group has switched on.
    flags: &'static [&'static str],
    /// Whether they administer their own team's rules.
    admin: bool,
}

fn directory_row(caller: &str) -> Option<Person> {
    match caller {
        "dana" => Some(Person {
            tenant: "qa-india/requestly",
            group: "qa-india",
            sub_group: "requestly",
            user_id: "u-10441",
            plan: "enterprise",
            flags: &["gpu-machines", "long-runs"],
            admin: true,
        }),
        "kiran" => Some(Person {
            tenant: "qa-india/requestly",
            group: "qa-india",
            sub_group: "requestly",
            user_id: "u-10442",
            plan: "enterprise",
            flags: &["gpu-machines", "long-runs"],
            admin: false,
        }),
        "ci-nightly" => Some(Person {
            tenant: "qa-india/automation",
            group: "qa-india",
            sub_group: "automation",
            user_id: "svc-771",
            plan: "trial",
            flags: &[],
            admin: false,
        }),
        _ => None,
    }
}

/// What a sub-group has written down. A real deployment reads this from wherever its teams
/// keep it — a repository, an object store, a table. It is **text**, because the decision
/// is a model reading everything they wrote, so where it comes from changes nothing.
fn rules_for(tenant: &str) -> String {
    let shared = "\
## How we allocate

Nightly and scheduled work is routine: hold it to the standing limit, whatever it asks
for. There is always tomorrow.

An engineer bisecting a live outage may have the maximum, but only if `incident` names
the outage. An assertion of urgency without one is not an incident.

Machines are not free. Prefer the cheapest that can do the work, and if today's budget is
more than three quarters spent, hold everything except incidents to the standing limit.

## What our plan and our flags mean

The `plan` and `sub_group` in the attested section come from the directory, not from the
requester, so they can be relied on.

A `trial` plan is for trying things: two machines, and never more, whoever asks and
whatever they say.

`gpu-machines` in `flags` means this sub-group has been enabled for GPU hardware. Without
it, refuse any request naming a gpu capability and say that the team is not enabled for
it — do not silently give them something else.

`long-runs` means a slow suite is expected here, so a large ask for a scheduled run is
not by itself suspicious.
";
    format!("FILES\n  policy.md\n\nCONTENTS\n--- policy.md ---\n# {tenant}\n\n{shared}\n")
}

#[async_trait]
impl Directory for Hosted {
    async fn look_up(&self, caller: &str) -> Result<Option<Tenancy>> {
        let Some(who) = directory_row(caller) else {
            // Never heard of them. Different from "not allowed", and cm says it
            // differently — one needs onboarding, the other needs permission.
            return Ok(None);
        };

        // The whole point: an organisation's own shape, as pairs. cm carries these into
        // the prompt and never reads them; the policy above reads them by name.
        let mut facts = BTreeMap::new();
        facts.insert("group".into(), who.group.into());
        facts.insert("sub_group".into(), who.sub_group.into());
        facts.insert("user_id".into(), who.user_id.into());
        facts.insert("plan".into(), who.plan.into());
        facts.insert("flags".into(), who.flags.join(", "));

        // Numbers this deployment enforces itself, whatever the prose argues. A trial
        // plan is not talked out of being a trial plan.
        let (standing, ceiling, credits) = match who.plan {
            "enterprise" => (4, 24, 4_000),
            // Two machines is the plan, not the budget: a trial that ran out of credits
            // by lunchtime would look the same from outside and mean something else.
            _ => (1, 2, 400),
        };

        Ok(Some(Tenancy {
            tenant: who.tenant.into(),
            // A real deployment would check the account is active and not in arrears.
            may_ask: true,
            // From the directory, never from the rules.
            may_write: who.admin,
            facts,
            rulebook: rules_for(who.tenant),
            standing,
            ceiling,
            lifetime: 1_800,
            budget: Some(Budget { credits, window: 86_400 }),
        }))
    }

    async fn spent(&self, _tenant: &str, _window: u64) -> Result<u64> {
        // A real one asks its billing service. Zero here so the example needs no state.
        Ok(0)
    }

    async fn charge(&self, tenant: &str, entry: &budget::Entry) -> Result<()> {
        // Called after the machines are already back, so a failure here loses money
        // rather than capacity — log it loudly, never refuse over it.
        println!("bill {tenant}: {} credit(s) for {}", entry.credits, entry.reservation);
        Ok(())
    }

    async fn write_rules(&self, tenant: &str, up: &proto::Upload) -> Result<Vec<String>> {
        // Authority is already checked — `may_write` said so. What is left is validating
        // the contents and storing them, which is this deployment's business. A real one
        // would write to the same store `rules_for` reads, atomically.
        anyhow::bail!(
            "this deployment keeps {tenant}'s rules in its own store; upload through the \
             usual review, not through {} file(s) over the wire",
            up.files.len()
        )
    }

    async fn roster(&self) -> Result<Vec<Listed>> {
        Ok(["qa-india/requestly", "qa-india/automation"]
            .into_iter()
            .map(|tenant| Listed {
                tenant: tenant.into(),
                budget: Some(Budget { credits: 4_000, window: 86_400 }),
                unread: None,
            })
            .collect())
    }

    fn describe(&self) -> String {
        "the example hosted directory (groups, sub-groups, plans, flags)".into()
    }
}

fn main() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(cyberium::controller::run(Box::new(Hosted)))
}
