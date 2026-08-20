---
title: Tenants
parent: Guide
nav_order: 6
---

# Tenants

```sh
cm tenant add <name> [--ceiling N] [--credits N] [--window SECS]
                     [--member <alias>]... [--admin <alias>]... [--note <text>]
cm tenant list
```

A tenant is who a controller serves. **Always** — self-hosted too, where a tenant is usually
a team rather than a company. Keeping the model the same in both cases is cheaper than having
two.

```sh
cm tenant add payments \
    --ceiling 3 --credits 400 --window 86400 \
    --member dana --member kiran --member ci-nightly \
    --admin dana \
    --note "checkout and billing"
```

## Two files, two owners

This is the whole design, and everything else follows from it.

```
tenants/payments/
  tenant.toml     ← yours. What they may have.
  policy.md       ← theirs. What they do with it.
  nivedanas/…     ← theirs. Whatever else they write.
  spend.log       ← the controller's. What they used.
```

`tenant.toml` is **host-owned** because every field in it is a thing a tenant would set
generously about itself. A tenant that could name its own members could claim somebody else's
callers, and with them somebody else's budget.

```toml
ceiling = 3
members = ["dana", "kiran", "ci-nightly"]
admins  = ["dana"]
credits = 400
window  = 86400
note    = "checkout and billing"

[facts]
plan  = "enterprise"
team  = "payments"
```

Editing it takes effect without a restart. So does adding a tenant.

## Members and admins

**Members** may plead — run tests, spend the budget. **Admins** may also change what the
tenant has written down, via [`cm upload-policy`](uploading-policy.html).

An admin is automatically a member: somebody trusted to write the rules is trusted to run a
test under them, and two lists to keep in sync mostly produces the bug where one is
forgotten.

**Absent `admins` means nobody, not everybody.** A tenant whose admins were unset would hand
its own rules to whoever runs tests, which is the exact thing the field answers:

```
$ cm upload-policy cm-c@acme .
refused: tenant `payments` has no admins, so nobody may change its policy —
         the host sets them in tenant.toml
```

This is the one thing about a tenant that is **not** up for interpretation. Everything else —
who may use which plea, whether free text counts, what `incident` is worth — is a sentence in
their own policy. But if a policy named its own admins, anybody who could edit it could add
themselves. Authority over a rule cannot come from the rule.

## How a caller becomes a tenant

There is no signup, no account, no token. A caller's tenant is the **alias in their verified
ticket** — minted by the controller's own sirji, not asserted by the caller. That is why
multi-tenancy needed no new credential and no new machinery: the answer to "who is this"
already existed.

`members` maps several aliases onto one tenant, which is what makes a tenant a team.

{: .note }
A tenant's *name* is not necessarily anybody's alias. `cm tenant add` writes a folder called
`payments`; the callers are `dana` and `kiran`. Two different lookups, and confusing them
made `cm tenant add` unable to find what it had just written. Found by running it, not by the
tests — every test used tenants whose name happened to be their member.

## Facts: what you attest about them

```toml
[facts]
plan      = "trial"
group     = "qa-india"
sub_group = "requestly"
```

These arrive in the model's prompt as **attested** — beside the caller's identity, in a
different section from anything the caller typed:

```
ATTESTED (proven — the requester cannot influence any of this)
tenant: payments
caller: dana
group: qa-india
plan: trial
sub_group: requestly
```

cyberium attaches no meaning to any of it. The point is that a policy can then say

> Sub-groups on the trial plan get at most two machines, whatever they ask for.

and *mean* it, because `plan` is not something a caller can claim. There is a test that the
same key sent by a caller lands only in the declared half and never in the proven one.

This is also how an access hierarchy or a feature-flag system reaches a policy without
cyberium growing code for either — see [your own controller](custom-controller.html) for
filling the same map from a real directory instead of a file.

## When a tenant's file will not parse

A malformed `tenant.toml` **skips that tenant** rather than falling back to defaults. Defaults
would put a ceiling and a budget in force that nobody chose, and nothing would look wrong.

If the tenant was already loaded, it keeps serving on its last good copy — a typo should not
take a paying team offline — and the mismatch shows up where an operator looks:

```
$ cm admin spend
  !! payments: configuration not read, still running on the last good copy
     — parsing tenant.toml: TOML parse error at line 10, column 10
```

{: .warning }
A key written after a `[table]` header belongs to that table. Appending `admins = ["dana"]`
below `[facts]` makes it `facts.admins`, which is how this behaviour was discovered — the
tenant ran with no admins at all and every other symptom looked normal.
