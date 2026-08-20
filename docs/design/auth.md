---
title: Identity and access
parent: Design notes
nav_order: 4
---

# Who is asking, and who says so

How cm decides that a request is genuinely from who it claims. The general shape is
sirji's [standing pattern](https://github.com/amitu/sirji/blob/main/patterns/standing.md);
this is what cm does with it.

> ## Status: designed, mostly not built
>
> Worth stating before anything else, because this document reads like a
> description and is largely a plan.
>
> **Built today:** a caller is a sirji device of its own sirji, and presents a
> sealed ticket its parent minted. The controller verifies one signature and
> learns an alias from it. That is the whole of it, and it works.
>
> **Not built:** OIDC, `cm auth login`, enrolment, the context JSON, per-org
> policy, the plan tier, and the two credential tiers. None of it exists in the
> binary.
>
> The consequence worth knowing: **the controller is single-tenant.** It holds one
> `policy.md`, read at startup, and every admitted caller is weighed against it.
> Everything below that says "per organisation" describes where this is going, not
> where it is.

## No shared secrets

cm has none, and does not need any. A shared secret is only required when an actor
is **both ephemeral and unattested**, and that set turns out to be empty:

| actor | ephemeral | how it authenticates |
|---|---|---|
| cloud CI runner | yes | per-request OIDC, bound to a one-off key |
| developer laptop | no | keypair, enrolled once |
| on-prem build server | no | keypair, enrolled once |
| worker | no | keypair, enrolled once |

**Everything durable is a keypair. Attestation exists only where there is nothing
to enrol.** A build server that has run for three years is not ephemeral, whatever
category it files under — the property that matters is continuity, not whether the
word "CI" appears in its name.

## Ephemeral: cloud CI

A runner generates a keypair for the run, asks its platform for an OIDC token, and
throws the key away when the job ends. Nothing is enrolled, so the controller's
roster does not grow by one entry per build.

Where the platform allows a custom audience — GitHub does — the **one-off public
key goes in the audience**. That makes the token useless to anyone who scrapes it
from a build log, because the matching private key was never written down and no
longer exists. Same caller-binding property a sealed ticket has, from a credential
cm did not issue.

The controller verifies signature, issuer, audience and expiry against the
provider's published keys, then maps the claims to an organisation it knows.

**Cache the provider's keys aggressively.** Verification needs egress to the
provider, and a network that filters egress is exactly the kind of network this
runs on. Keys rotate slowly; a stale-but-cached key set is a better failure mode
than an outage.

## Durable: enrolment, once

Everything else holds a keypair, and the only question is how it becomes known.
Enrolment happens through the anonymous door — a stranger on the published
handshake key may do exactly one thing, and everything else is refused.

```
cm auth login --at <service>    mint a key for this service, prove who you are, enrol it
cm auth status                  which services this machine is enrolled with, as whom
cm auth logout --at <service>   the service forgets the key; delete it locally
```

The service advertises which proofs it accepts, so the client implements
**protocols rather than providers**: one RFC 8628 device-flow implementation covers
any standards-compliant identity provider, discovered through its
`.well-known/openid-configuration`. Adding a provider is server-side configuration
and needs no client release.

A fresh keypair is minted **per service** — the substrate mints a peer key per
relationship so no two peers can correlate you, and a service is no different.

After enrolment there is **no session**: no token, no expiry, no refresh. Later
requests are keypair-authenticated. Revocation is the service forgetting the key.

## Declared and attested

Everything reaching a policy decision is labelled with its provenance, and the two
never merge:

```
attested   org, actor, repo, ref        the caller cannot lie about these
declared   alias, context JSON, count   what the caller says it is doing
```

Collapsing them would be simpler and worse, because **the gap between them is
information**. A request declaring itself a nightly while attested as arriving from
a pull request has told you something, and only a system that kept them apart can
notice.

cm hardcodes no cross-checks between the two. What counts as a suspicious mismatch
varies per organisation, and that judgement is what `policy.md` is for. The code's
job is to keep provenance honest and hand both sides over intact.

## The context JSON

A nivedana carries an arbitrary, org-defined JSON object beside its alias. Policy
prose is written to interpret it, and the two evolve together as an organisation's
needs get more complicated. cm knows none of the keys.

This is safe for a reason that only appears once the controller is multi-tenant:
**the JSON never crosses an organisation boundary.** A request from acme is weighed
against acme's policy, and acme's JSON is produced by acme's own workflows. The
organisation writes both sides. What one tenant sends cannot influence another,
which is held by the outer entitlement ceiling instead.

Residual risk is intra-org — a fork PR, or human-typed text finding its way into
the object — and three things already handle it: the attested facts sit beside it
so a mismatch is visible; the deterministic clamp means a successful injection wins
at most the organisation's own ceiling; and the object is fenced and labelled as
caller-supplied wherever it is read.

The discipline that follows, and it belongs in every Action template: **build the
JSON from workflow context, not from comment bodies.** A workflow file is reviewed;
a comment is not. cm cannot tell them apart — the platform signs claims about the
run, not about a payload we chose — so this is a convention, not an enforcement.

Three limits, for reasons that are not obvious:

- **Cap the size and reject rather than truncate.** A silently shortened object
  changes meaning without saying so, and a decision rests on it.
- **Render deterministically**, sorted keys and stable formatting, because the
  policy snapshots depend on identical requests producing identical prompts.
- **No key has protocol meaning.** `role` is a convention an organisation may
  adopt; cm's code never reads it.

## Three tiers of entitlement

Only the middle one is cm's own invention:

| tier | set by | answers |
|---|---|---|
| the plan | the host | how much of a shared fleet this organisation may take |
| `policy.md` | the organisation | how that is divided between its repos, teams and incidents |
| the fleet | reality | which machines are actually free right now |

A hosted deployment's outer ceiling already exists as whatever the customer bought,
so an organisation authoring its own `policy.md` is dividing an allowance rather
than setting one. Without that tier, uploading a policy would be uploading a quota.

## Two credentials, two repositories

For an organisation whose policy lives in a repository:

| credential | lives in | authorises |
|---|---|---|
| policy-admin | the `<org>/cyberium` repo | replacing the policy bundle |
| ordinary | every other repo | asking for machines |

Separate for the obvious reason: a repository that can ask for machines must not be
able to raise its own limit.

Both are OIDC identities rather than secrets — the distinction is which repository
the claims name, and what the controller lets that repository do.

## Bootstrap

The one part that looks circular and is not:

```
an account with the host        (a human, with a plan)
        │  registers "our identity provider org is acme"
        ▼
trust for acme/*  →  every repository's CI works, with nothing stored
        │
        └─ the same account registers the policy repository
```

One human action per organisation, then nothing to rotate.

A hosted, multi-tenant service does have accounts. The substrate has none, and that
claim should stay scoped to the substrate rather than quietly stretched to cover
the product.
