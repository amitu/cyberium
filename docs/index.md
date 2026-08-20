---
title: cyberium
layout: home
nav_order: 1
---

# Machines for tests, handed out by a policy you wrote in English

A developer asks a fleet for machines. A controller reads what that developer's team has
written down — in prose, in a folder, in their own words — and decides how many they get.

```sh
$ cm t cm-c@acme --plea nightly-regression --count 6 --need linux
declaring: plea=nightly-regression
countered: 2 — a nightly suite has all night, so this is held to the standing limit
```

Nobody wrote that rule as code. It is a paragraph in a file:

```markdown
Nightly and scheduled work is routine: hold it to the standing limit, whatever it asks
for. There is always tomorrow.

An engineer bisecting a live outage may have the maximum, but only if `incident` names
the outage. An assertion of urgency without one is not an incident.
```

{: .highlight }
**cyberium is early.** Everything on this site is running and verified end to end unless a
page says otherwise, and pages say otherwise where it is true. The credential story is the
main thing still unbuilt — see [Identity and access](design/auth.html).

---

## Start here

- **[Install](guide/installing.html)** — binaries, or build it yourself
- **[Quickstart](guide/quickstart.html)** — a controller, a worker and a test run, on one machine
- **[Run a suite](guide/running-tests.html)** — `cm t` in full: sharding, checkouts, artifacts
- **[Playwright](guide/playwright.html)** — `npm test` distributes itself, and your config is untouched

## The policy

- **[Writing a policy](guide/policy.html)** — the folder is the policy, and a model reads all of it
- **[Testing a policy](guide/testing-policy.html)** — `cm policy-test`, to check prose the way you check code
- **[Shipping a policy](guide/uploading-policy.html)** — `cm upload-policy`, and who is allowed to
- **[Budgets](guide/budgets.html)** — credits, rates, and what a machine-minute costs

## Running a fleet

- **[Workers](guide/workers.html)** — capabilities, rates, and cleaning up between tenants
- **[Tenants](guide/tenants.html)** — teams, members, admins, and the facts you attest about them
- **[Operating a controller](guide/operating.html)** — admin devices, and looking inside a running one
- **[Your own controller](guide/custom-controller.html)** — the library seam, for callers who live in a directory
- **[Reference](guide/reference.html)** — every command, flag, environment variable and file

## Why it is built this way

Four [design notes](design/policy.html) carry the reasoning, each opening with what does and
does not exist yet. They are worth reading before disagreeing with a decision:
[policy](design/policy.html) — including [the answer's
schema](design/policy.html#the-answer-is-a-contract-not-a-conversation) and the
[post-processor](design/policy.html#the-post-processor) — [testing a
policy](design/policy-testing.html), [budgets](design/budget.html), and
[identity](design/auth.html).

---

## The three shapes

Everything is a **sirji device**. None of them holds an account, a password or a shared
secret.

**`cm controller`** owns the whole picture — which machines exist, what they can do, who
holds them, when to take them back. It is the only thing with a view of the fleet.

**`cm worker`** offers one machine. It finds the controller through their shared parent,
registers, and holds the connection open. That connection *is* its availability: no
heartbeat, no timeout arithmetic, no stale roster.

**`cm test`** asks. It resolves the controller, pleads, and then talks to the granted
machines **directly** — the controller allocates but never carries the work.

Workers never speak to each other. They have nothing to say: everything needing a view of
the whole fleet lives in exactly one place.

## What makes it different

**The decision is not code.** Every plea is weighed against the tenant's whole folder by
one model call, and the number that comes back *is* the allocation. Not a fast path for
easy requests and a model for the rest — how many machines a request deserves is the
question the policy was written to answer.

**But the model is never the gate.** The answer is clamped to a ceiling a human wrote, to
what was actually asked for, to what is free, and to what the budget can buy. Every one of
those limits is also *shown* to the model, so a clamp that fires is logged as a defect in
the policy rather than treated as the normal way an answer gets made.

**And a broken model fails the request.** No key, a timeout, an API having a bad day: the
request fails and says nothing was decided. Substituting a number would hand out machines
on cyberium's authority instead of the organisation's — invisibly, at the exact moment the
component that reads the policy has stopped working.

**Identity is a key, not an account.** No signup, no tokens to rotate, no shared secret
between a test runner and a fleet. A caller's team is the alias their own organisation's
sirji minted into a ticket, which is why multi-tenancy needed no new credential.
