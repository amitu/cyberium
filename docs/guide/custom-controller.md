---
title: Your own controller
parent: Guide
nav_order: 12
---

# Your own controller

`cm` keeps callers in folders: a directory per tenant, terms in `tenant.toml`, spend in a
ledger. That is the whole answer for an organisation with no identity service — and the wrong
answer in every particular for one that already has groups, sub-groups, user ids and a
feature-flag system. None of that shape belongs in an open-source allocator.

So the crate is a library as well as a binary, and what a controller *knows* is a trait.

```toml
[dependencies]
cyberium = { git = "https://github.com/amitu/cyberium" }
async-trait = "0.1"
```

```rust
#[async_trait]
pub trait Directory: Send + Sync {
    async fn look_up(&self, caller: &str) -> Result<Option<Tenancy>>;
    async fn spent(&self, tenant: &str, window: u64) -> Result<u64>;
    async fn charge(&self, tenant: &str, entry: &budget::Entry) -> Result<()>;
    async fn write_rules(&self, tenant: &str, up: &proto::Upload) -> Result<Vec<String>>;
    async fn roster(&self) -> Result<Vec<Listed>>;
    fn describe(&self) -> String;
}
```

```rust
fn main() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?
        .block_on(cyberium::controller::run(Box::new(MyDirectory)))
}
```

That binary takes the same `cm init`, the same `CM_HOME`, the same `CM_MODEL_KEY`. Everything
else is shared — the protocol, the fleet, the model call, the clamps, the budget arithmetic —
and **`cm test` and `cm worker` need no customisation at all.** What varies between deployments
is what a controller knows, never how machines are asked for or handed over.

## What you return

```rust
Tenancy {
    tenant: "qa-india/requestly".into(),   // who pays, and whose rules apply
    may_ask: true,                          // is this account active?
    may_write: person.admin,                // may they change the rules?
    facts,                                  // group, sub_group, plan, flags
    rulebook: rules_for(&tenant),           // their rules, as text
    standing: 4,
    ceiling: 24,                            // every cap that applies, folded in
    lifetime: 1_800,
    budget: Some(Budget { credits: 4_000, window: 86_400 }),
}
```

## Three things the trait deliberately lacks

**No `is_enabled(feature)`.** A group hierarchy and a feature-flag system arrive as `facts`,
get [attested in the prompt](tenants.html#facts-what-you-attest-about-them), and are read by
the policy:

```markdown
`gpu-machines` in `flags` means this sub-group has been enabled for GPU hardware. Without
it, refuse any request naming a gpu capability and say the team is not enabled for it — do
not silently give them something else.
```

So `group`, `sub_group`, `plan` and `flags` are pairs cyberium carries and never interprets,
and a rule like *"a trial plan gets two machines, whoever asks"* is a sentence rather than a
branch. A gate in the trait would be cyberium growing an opinion about a vocabulary that is not
its own.

**No parsed policy.** `rulebook` is a `String`, because the decision is a model reading
everything a team wrote. Keep your rules in a database, an object store or a git repository —
the decision cannot tell the difference.

**No `may_write` from the policy.** It is a `bool` you answer from your own directory. If a
policy named its own admins, anybody who could edit it could add themselves. Authority over a
rule cannot come from the rule.

## A working one

[`examples/hosted/`](https://github.com/amitu/cyberium/tree/main/examples/hosted) is a whole
custom controller in one file: a made-up identity service with plans and flags, hard-coded so
the example needs no infrastructure.

`scripts/hosted.sh` runs it against real workers and a real `cm t`, which is the part
compiling does not prove:

```
== start the custom controller — note there is no tenants/ folder at all
  callers known from: the example hosted directory (groups, sub-groups, plans, flags)
  2 tenant(s): qa-india/requestly, qa-india/automation
  ok: no tenants/ directory exists

== dana is on the enterprise plan — her ceiling comes from the directory, not a file
  would get 4 machine(s) — …

== ci-nightly is on a trial, in the same organisation — 2 is all it can have
  would get 2 machine(s) — …

== lee is not in the directory — refused differently, because the fix is different
  Error: denied: lee is not a tenant of this controller
```

Two callers in the same organisation, different ceilings, and neither number from any file on
the controller.

## Other seams

**The model endpoint** needs no trait: `CM_MODEL_URL` points at anything speaking the same
shape, which is how the repository's own scenarios run against a stand-in.

**`cyberium::testing::scratch`** is public rather than test-only, because anybody writing a
controller needs collision-free scratch directories and the trap it documents — pid plus
nanoseconds is *not* unique — is not obvious enough to leave to rediscovery.
