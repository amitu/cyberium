# policy.md, and what happens to it

Where the organisation's rules live, how they get there, and who may change them.

> **Status.** The file, its two halves, and the deterministic gate are built and
> running. `nivedanas/`, the model call, `cm test-policy`, `cm upload-policy` and
> per-organisation policy are designed and **not built** — today the controller
> reads one `policy.md` from `--root` at startup and weighs every caller against it.

## The file has two halves on purpose

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

Everything after it is prose, to be weighed by a model against the reason a caller
gave. That half is loaded today and not yet reasoned over — the controller applies
the grants and the standing limit and says so in its rationale rather than
pretending.

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

**Only the middle one exists today**, because a single-tenant controller has no host
above it. It matters for the hosted case: without an outer ceiling, an organisation
authoring its own `policy.md` would be authoring its own quota, and `standing_limit:
10000` is a valid file.

## Tenancy, plainly

**Today:** one controller, one `policy.md`, one set of rules. Anyone the
organisation's sirji has a relationship with is weighed against the same file. That
is the right shape for an organisation running cm on its own machines, and it is
what the fleet script demonstrates.

**Designed:** a hosted controller serving many organisations, each with its own
policy bundle, selected by which organisation the caller was attested as belonging
to. The pieces that changes:

- `Control` holds a policy **per organisation** rather than one
- a bundle is `policy.md` + `nivedanas/` + `policy-tests/`, versioned together
- the plan tier appears above it
- and a caller's organisation has to be *attested*, which is what
  [auth.md](auth.md) is about

None of that is built. It is written down because the single-tenant code should not
acquire assumptions that make it hard — which is why `weigh` already takes the asker
rather than reading an ambient identity.

## Getting a policy onto a controller

Today: put the file where `--root` points, and start the controller. Changing it
means restarting.

Designed, and the shape matters more than the commands:

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
