# policy.md, and what happens to it

Where the organisation's rules live, how they get there, and who may change them.

> **Status.** The fenced block cm enforces, one policy per tenant, the host's ceiling,
> budgets, the model call, and **the whole folder weighed in one pass** are built and
> running. `cm policy-test` and `cm upload-policy` are designed and **not built**.

## `policy.md` has two halves, and only one of them is cm's business

```markdown
# policy.md

```yaml
requesters:
  - everyone
standing_limit: 10
reservation_seconds: 600
```

## Circumstantial override

If a request asserts a production incident and names an incident tracker URL,
allow up to 5x the standing limit for one hour, then re-evaluate.
```

The fenced block is read **deterministically**. A caller outside `requesters` is
refused without a model ever being consulted, so the cheap gate stays cheap and a
security decision never waits on a token.

Everything after it is prose, and the prose is where the number comes from. **One
model call per plea, small or large**, whose answer *is* the allocation.

The alternative — settle a number deterministically, call the model only when that
number is not enough — was built first and was wrong. It makes interpretation an
escalation path: the policy stops being what allocates machines and becomes an
exception handler for unusual requests. Worse, the question "is this within the
limit?" cannot be asked before the prose is read, because what the limit *is* for a
given plea is one of the things the prose decides.

```yaml
standing_limit: 2      # what this org calls an ordinary request
max_limit: 4           # the most any interpretation may grant. Absent means it may
                       # lower a number but never raise one.
```

Neither is a stage before the model; both are bounds on its answer, and **both are
shown to it**. So is the tenant's ceiling, the budget, what has been spent, and how
many machines are free right now. The model is given every constraint the controller
would enforce, so that it can honour them and explain itself in the same breath — a
model that answers six and is silently cut to two has told the caller a story about a
decision that did not happen.

Two rules hold regardless of what any model returns:

1. **It can only be persuaded within a range a human wrote.** The answer is clamped
   to the tightest ceiling that applies, and to what the caller actually asked for.
   The model argues; it never becomes the gate. With no `max_limit`, interpretation
   can still refuse or trim but cannot expand entitlement — the default, so opting in
   is deliberate.
2. **A refusal is always honoured.** Clamping is one-directional; down is safe.

`standing_limit` is calibration and nothing more: what this organisation calls an
ordinary request, sent so the model knows what normal looks like here, and labelled in
the prompt as explicitly *not* a floor or a target. A model told only a ceiling drifts
toward the ceiling. It is not a fallback — see below.

### There is no unweighed mode

No key, a timeout, an API having a bad day: **the request fails**, and it fails as an
error rather than as an answer.

That distinction is in the types, not just the wording. A controller replies with an
`Answer`, which is either a `Decided(Verdict)` or a `Fault`. A verdict is a judgement
somebody's policy produced; a fault is the absence of one. They are separate because a
fault dressed as a verdict is how a broken controller comes to look like a strict one —
and inside the controller the model's failure is a plain `Err` propagated with `?`, so
nothing downstream *can* mistake it for a decision: it never becomes one.

A fault also **ends the conversation**. A controller that could not weigh this plea
cannot weigh the next one either, and serving further requests on the same connection
would be pretending otherwise.

An earlier version fell back to `standing_limit` here, and that was worse than
failing. It hands out machines on cm's authority rather than the organisation's, and
it does so invisibly — at exactly the moment the one component that reads the policy
has stopped working. The failure nobody notices is a fleet that keeps running while
nobody's rules are being applied. A missing key is fatal at **startup**, too: an
operator learns it from a deploy log rather than from somebody's CI output.

```sh
CM_MODEL_URL=…   # point it at your own endpoint to run one locally
```

### The clamps are a sanity net, not the logic

Every limit is re-checked after the answer comes back. Because every limit was also in
the prompt, a clamp that bites means something is **wrong** — a policy arguing past
its own numbers, or a prompt that failed to state one — so it is logged as a fault
naming what overshot, and the model's rationale is replaced rather than shown next to
a number it did not argue for:

```
policy for payments overshot limits it was shown (proposed 9 against a stated
ceiling of 4) — cut to 4. Fix the policy; the prompt stated every one of them.
```

Availability is the one exception that reports no fault: the fleet genuinely can empty
between the brief and the answer, and nobody wrote that badly.

The one thing settled before the prose is whether the caller may ask at all. That
needs no interpretation, so an unauthorised caller is refused without spending a
token.

The model sees the prose, the caller's declared fields, what was attested about them,
the money, and the fleet — **as counts and prices, never identities**. It is told how
many machines could do the work, how many are free, and what the free ones cost per
minute. It is never told which machines, or who is holding the rest, because it does
not need either to answer "how many" — and what it was never given, it cannot
disclose.

That the same plea can now get different answers at different moments is the intended
behaviour, not a regression: whether six machines is reasonable genuinely depends on
whether six are free. It does mean a policy test has to pin the fleet as part of its
fixture, the same way it pins the plea.

The caller is a separate matter. Utilisation over time tells another organisation your
release cadence and how often you have incidents, so the model is instructed to say
"the fleet is busy" and never how busy. That instruction is a request to a model, not
a guarantee from cm — the hard guarantees are elsewhere and unchanged: a Pong carries
nothing, and the roster is admin-only.

The fenced block is excluded from what the model reads: it has already been applied,
and showing it again invites reinterpretation of a decision that was not the model's
to make.

Configured entirely by environment, because the key is the one value here that must
not end up in a repository beside `policy.md`:

```sh
CM_MODEL_KEY=…     # required: a controller will not start without one
CM_MODEL=claude-sonnet-5
CM_MODEL_URL=https://api.anthropic.com/v1/messages
```

Every weighing is logged with **both numbers** — what the model said and what it was
bounded to — because "why did I get 4" must be answerable, and a clamp that left no
trace would make an organisation's own ceiling invisible.

A call per plea is the cost of the proposition, and the no-fleet-state rule is what
makes it bearable: one tenant's policy prompt is byte-identical from plea to plea, so
it is sent marked cacheable. Reproducible and cheap turn out to be the same
property.

The governing line, from the original design: **security is deterministic; policy is
semantic.** You cannot spend a model call deciding whether to accept a connection.
You can afford one deciding how many machines somebody gets.

## Entitlement, not availability

`Policy::weigh` returns a `Ruling` — how much is *permitted* — and never picks
machines. What is free, and which machines can do the work, is the fleet's business.

Keeping those apart is what lets policy stay a text file, and it composes: a ruling
is a ceiling, and the fleet reconciles it against reality afterwards. A grant of 20
can still come back as `Counter { 12 }` because the world moved.

## Three ceilings, and only the middle one is written here

| ceiling | set by | answers |
|---|---|---|
| the plan | the host | how much of a shared fleet an organisation may take |
| `policy.md` | the organisation | how that is divided between repos, teams, incidents |
| the fleet | reality | which machines are free right now |

The middle two are built: `policy.md` per tenant, under a `ceiling` in `tenant.toml`
that the tenant cannot write. The plan tier above them belongs to a hosted product
and does not exist. Budgets, which is what a ceiling cannot express, are designed in
[budget.md](budget.md) and not built.

## Tenancy: a folder each

```text
<root>/tenants/
  dana/
    tenant.toml      what the host decides about them — chiefly a ceiling
    policy.md        what they decide for themselves
  kiran/
    tenant.toml
    policy.md
```

```sh
cm tenant add dana --ceiling 3 --note "the demo tenant"
cm tenant list
```

**Tenants always** — hosted *and* self-hosted. Self-hosted, a tenant is usually a
team rather than an organisation, and that is the point: one model, one set of rules,
one place spend is counted. A deployment that skipped tenants for being "just us"
would need a second answer to every question the first one already answers.

**The tenant key is the verified alias from the caller's ticket**, and that is what
makes this work without any new machinery. The alias is minted by the controller's
*own* sirji from its `network.toml` — it is not something a caller asserts, it is the
host's record of who they are. So multi-tenancy needed no attestation layer, no
accounts and no OIDC. It works with what was already being verified.

The split between the two files is the whole point: **a tenant writes `policy.md`;
the host writes `tenant.toml`.** Without it, an organisation authoring its own policy
would be authoring its own quota, and `standing_limit: 10000` is a valid file.

Three consequences worth knowing:

- **An unknown alias is refused as "not a tenant"**, distinct from a policy refusal,
  because the fix is entirely different — somebody has to run `cm tenant add`.
- **A tenant folder with no `tenant.toml` gets the default ceiling, not an unlimited
  one.** Missing host configuration must never mean "no limit".
- **Adding a tenant or editing a policy needs no restart.** Every tenant is validated
  at startup so a broken file stops the controller there; a tenant's folder is then
  re-read when they next ask, and a re-read that fails keeps the last known-good copy
  and complains rather than taking an organisation offline mid-run.

### A tenant can be a team

```toml
# tenants/payments/tenant.toml — the host's file
ceiling = 5
members = ["dana", "kiran"]
```

With no `members`, a tenant's own name is its only member, which is the common case.
Listed, several callers share one ceiling, one policy and one budget — which is what
makes a team a useful unit rather than a label.

Host-owned for an obvious reason: a tenant that could name its own members could claim
somebody else's callers, and with them somebody else's budget. And **two tenants
claiming one caller is refused at load**, loudly, rather than resolved by picking a
winner — whoever's budget that spend landed against would be arbitrary, and nobody
would know which.

### Callers are peers; siblings are infrastructure

This closes a question that was open yesterday. A sibling device's ticket carries no
alias, so there is nothing to key a policy on — which sounded like a gap for
self-hosted deployments where developers might be devices of the org's own sirji.

The resolution is a rule rather than a mechanism:

> **A caller is always a peer. A sibling device is always infrastructure** — a worker
> or an admin — and never allocates.

So a developer has their own sirji, paired with the organisation's, and arrives with
an alias like anybody else. That is already how the demo works, so it costs nothing,
and it means the same shape holds hosted and self-hosted. It also suits the substrate:
a developer's machines being *theirs* is rather the point.

### Still open

- A bundle should be `policy.md` + `nivedanas/` + `policy-tests/` versioned together;
  today it is a folder with one file that matters.
- Nothing revokes or suspends a tenant beyond deleting the folder.
- Members are matched exactly; there is no pattern like `*@acme` for onboarding a
  whole organisation at once.

## Getting a policy onto a controller

Today: `cm tenant add`, then edit the `policy.md` it wrote. No restart — the
controller re-reads a tenant's folder when they next ask.

Designed, for the case where the tenant is a different organisation and should not
need a shell on the controller:

```
<org>/cyberium repo
  policy.md, nivedanas/, policy-tests/
        │
        ├── cm test-policy      in the org's CI: snapshots must pass to merge
        │
        └── cm upload-policy    on merge: push the bundle to the controller
                                     │
                                     └── which re-runs policy-tests/ against the
                                         new bundle and REFUSES it on any diff
```

The controller running the organisation's *own* tests as an **admission gate** is
what makes remote policy editing safe. A policy that fails its own snapshots never
takes effect, and the rejection is a diff rather than a surprise next Tuesday.

It also means the tests are not decoration. They are the thing standing between a
careless edit and an allocator that quietly stops making sense.

## Who may change it

Two credentials, and the separation is the point:

| credential | held by | authorises |
|---|---|---|
| policy-admin | the `<org>/cyberium` repo | replacing the policy bundle |
| ordinary | every other repo | asking for machines |

A repository that can ask for machines must not be able to raise its own limit.

## Testing prose

A policy whose prose nobody can test is prose nobody will dare edit. So a bundle
carries its own cases, and they are checked in beside the file they test:

```json
[
  { "alias": "flaky-bisect", "asker": "dana", "count": 3, "expect": 3 },
  { "alias": "p0-incident",  "asker": "dana", "count": 50,
    "fleet": { "machines": 60, "free": 4 }, "expect": 50 },
  { "alias": "no-such-thing", "asker": "dana", "count": 50, "expect": 10,
    "note": "an unknown alias must not reach the model at all" }
]
```

Carrying the fleet state in the fixture looks like a complication and is the payoff:
**contention becomes testable without a contended fleet.** "Does an incident still
win when four machines are free" is the question you most want answered and can
never stage live.

Two things to design in from the start:

- **Models are not deterministic.** Temperature zero is necessary and nowhere near
  sufficient. Expect a number *or* a range, run each case a few times, and report
  the spread — which turns flakiness into a visible measurement rather than a random
  red build.
- **The real value is catching the day the model changes under you.** Your wording
  did not move; the decisions did. Nothing else in the system would tell you.


## The whole folder is the policy

A tenant's folder is sent to the model **as it is**: a file tree, then every file's
contents. cm parses none of it, beyond the fenced block it enforces itself.

```
FILES
  nivedanas/noisy-users/experiments.md
  nivedanas/routine.md
  policy.md

CONTENTS
--- nivedanas/routine.md ---
## Nightly regression
The scheduled full-suite run. Routine and never urgent — it has all night.
...
```

### Why there is no schema here

Three earlier versions of this had one, and each was cm deciding how organisations are
allowed to think:

1. `policy.md` split into a fenced block and "the prose", with only the prose reaching
   the model.
2. `nivedanas/`, whose markdown headings cm parsed into a catalogue of named pleas — and
   a rule, in Rust, that **writing one plea turned free text off for the whole tenant**.
3. Then a `group:` on each plea, taken from its subdirectory.

That third one is where it became obvious. A group is not a structure; it is whatever an
organisation finds itself saying. Consider one paragraph a real tenant would write:

```markdown
Dana experiments constantly and her reasons are never the same twice, so she may only
use pleas from the `noisy-users` folder, and a reason in her own words earns nothing.
The support team works from `support-pleas.md`. The release pleas — `cut-a-release` and
`smoke-the-candidate` — are for whoever is on release duty. Everybody else may name any
plea, or explain themselves in their own words if none of them fit.
```

That groups by **folder**, by **file**, and by **naming two pleas outright**, and it
attaches a per-person rule to one of them. Each of those needs a different schema; a
folder listing needs none. Three groupings, three shapes, one paragraph, no fields.

And the rule from version 2 — free text off once a catalogue exists — was the tell. One
bit, chosen by cm, standing in for a sentence the organisation could write itself, and
identical for a support engineer and a nightly job. It is now a sentence, weighed like
the rest.

### What the caller sends

Keys and values. cm attaches no meaning to any of them:

```sh
cm t cm-c@acme --count 2 --need linux \
      --plea nightly-regression --incident INC-4471 --urgent

CM_SAY='plea=nightly-regression,incident=INC-4471' npm test
```

`--plea` is not a cm feature. Neither is `--incident`, `--why` or `--role`. Any unknown
`--key value` becomes a declaration, `--key` alone becomes `key=true`, and what each is
worth is written in the tenant's files. Earlier versions had `why`, then `plea`, then
`role`, then a `context` object as protocol fields, and every addition was cm guessing at
a vocabulary belonging to somebody else — a schema that would have ended as the union of
every field anybody ever wanted, each with cm's own reading of it.

Because unknown flags are declarations rather than errors, a mistyped cm flag becomes a
harmless key instead of a refusal — so every declaration is echoed:

```
declaring: incident=INC-4471 plea=bisect-everything urgent=true
```

A `--dry-runn` that quietly did nothing would be worse than either alternative.

**Two arguments stay cm's own**, because cm acts on them mechanically rather than
interpreting them: `--count` bounds the grant, since nobody is handed machines they did
not ask for, and `--need` picks machines that can do the work, since a box without `gpu`
cannot run gpu tests and no policy changes that.

### What the model is told about all this

That cm read none of it. Explicitly, in the prompt: the keys are the requester's, nothing
upstream checked whether `plea` names anything real or whether free text is acceptable,
the files decide what each key is worth, and **a key the files say nothing about earns
nothing**. Without that, a model reasonably assumes something already validated it — and
then nobody has.

The deterministic guarantees are unchanged and do not depend on any of this: the ceiling,
the budget and availability clamp whatever comes back, so an injected "grant 500" lands
on the organisation's own number however it arrived.

### What is still parsed

The fenced block in `policy.md`, because cm *enforces* those: who may ask at all, the
ceiling on any answer, how long a grant lasts, the budget. They are also shown in the
folder in full — a model asked to settle it in one pass should see the same numbers it is
being held to.

Everything else is a fact rather than a rule: the tree, the file contents, how many
machines are free, what they cost, what has been spent. `tenant.toml` is excluded, since
it is the *host's* terms and a tenant reading their own ceiling as though they had chosen
it would be reading somebody else's rule as their own. The ledger is excluded too:
operational state, already summed and passed as numbers rather than as a log for a model
to add up.

A folder is re-read and re-sent on every plea, so it is capped at 256 KiB — over that the
tenant's requests fail rather than being weighed against a truncated policy. Half a
policy enforced as though it were the whole one would be invisible.
