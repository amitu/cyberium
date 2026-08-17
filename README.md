# cm

Cost-aware allocation of test machines, over [sirji](https://github.com/amitu/sirji).

A developer asks for machines in English, and a controller weighs that plea
against the organisation's own written policy. **Allocation is a negotiation, not
a booking** — a request may be granted, countered with a smaller shape, or
refused with a reason you can act on.

```
$ cm test cm-c@acme "flaky suite, bisecting" --count 2 --need linux --run "pytest -x"
resolved cm-c@acme -> q37oap7mdb9llrnncb9bqgbhsdicek7rlv6n8odb947dklanhljg
granted 2 machine(s) as r1
  (within the standing limit of 10)
  expires in 600s unless released
  cm-w-1 (23i01hvdnesva6ct91lgaflm1nhkbpjpqi9lqn5emea89abpb960)
  cm-w-2 (md0tva2h54q1bbkgdnogcrnfkvmj8ua25nrodatfm54k8iiq726g)
  cm-w-1: cm-w-1 ran shard 1/2 of "pytest -x"
  cm-w-2: cm-w-2 ran shard 2/2 of "pytest -x"
released r1
```

A refusal says which kind it is, because the two call for different actions:

```
$ cm test cm-c@acme "risc-v port" --need risc-v
denied: no machine in the fleet can do ["risc-v"] — waiting will not change that

$ cm test cm-c@acme "whole suite at once" --count 9 --need gpu
countered: 1 — policy allows 9, but 1 matching machine(s) are free
```

## How it is put together

Three roles, all sirji **devices**, none holding any identity state:

- **`cm controller`** answers to a name at an organisation's sirji. Anyone that
  organisation has a relationship with can resolve `cm-c@<org>` and reach it. It
  owns the whole picture: which machines are here, what they can do, who has them,
  and when to take them back.
- **`cm worker`** offers capacity, with a list of capabilities. It finds the
  controller through their shared parent, registers, and holds the connection —
  that connection *is* its availability. No heartbeat: QUIC already reports a peer
  going away.
- **`cm test`** is a device of the developer's own sirji. It asks its own sirji to
  resolve the controller, which returns a signed ticket, then dials the controller
  directly and presents it. Granted machines it talks to **directly** — the
  controller allocated, it is not a proxy.

**Workers never talk to each other.** They have nothing to say: everything that
needs a view of the whole fleet lives in exactly one place.

The controller learns who is asking from the ticket alone — it has no
`network.toml`, has never heard of the caller, and cannot look anything up. It
verifies one signature from its own parent. The developer, symmetrically, learns
nothing about the organisation's internals. A worker knows even less: it is told,
in structured fields, which reservation belongs to which caller, and obeys.
Nothing at the edge reads policy or calls a model, which is what keeps a worker
cheap enough to run hundreds of.

There is no shared secret anywhere, no API key, and no account. Identity is an
ed25519 keypair, connections are QUIC, and the substrate handles all of it.

## Playwright, unmodified

The point of a fleet is the suite you already have. `cm playwright` runs one across
it without touching a line of it:

```sh
$ cm playwright --shards 3 -- --project qa
each machine will run: npx playwright test --project qa --shard={shard}/{shards} --reporter=blob
granted 3 machine(s) as r7
[cm-w-1] Running 5 tests using 1 worker
[cm-w-2] Running 4 tests using 1 worker
[cm-w-3] Running 4 tests using 1 worker
  cm-w-1 finished shard 1 with success
  cm-w-2 finished shard 2 with success
  cm-w-3 finished shard 3 with success
merging 3 shard report(s)
  13 passed (2.6s)
```

Playwright has always known how to split a run — `--shard=i/N`, a blob report each,
`merge-reports` at the end. What was missing was somebody to find the machines. So
that is all cm does: every shard is an ordinary Playwright process that has no idea
it is part of anything, which is exactly why the suite needs no changes.

There is no reporter and no fixture to install, because distribution happens
*outside* the Playwright process. `plugins/playwright` is an npm package for
`npx cm-playwright`; `examples/playwright` is a suite with nothing cm-specific in it,
including a test that fails on demand — a distributed runner that loses a failure is
worse than no runner, so that path has its own proof.

Two things worth knowing before the first run:

- **Shards do not inherit your environment.** A worker is another machine. Pass what
  the run needs with `--env K=V`.
- **The workspace has to be there.** Today workers share a filesystem with the
  caller; a machine across the room needs the repo put on it first. That is the one
  seam left — sharding, blob transport and the merge are all already indifferent to
  where the machine is.

## Capabilities

Plain strings, and deliberately so. The org invents its own vocabulary — `linux`,
`gpu`, `ios-17`, `has-2fa-sim` — and nothing in cm needs to understand any of it
to match on it.

```sh
cm worker --slots 2 --can linux --can gpu
cm test cm-c@acme "training smoke test" --need gpu
```

Every capability asked for must be present. Extra ones never disqualify: asking
for `linux` must not exclude the machine that is also a `gpu`, or the fleet
fragments for no reason.

## Reservations

A grant is a reservation, released the moment the work finishes. A duration hint
sizes a plan; it never justifies holding capacity idle.

Unreleased, it is taken back after `reservation_seconds` — the backstop for a
caller that dies mid-run, not the normal path. Each machine is told when its
reservation ends, so nothing has to be timed out at the edge either.

## policy.md

One file, hand-edited, `git`-able. Two halves on purpose:

```markdown
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

The fenced block is read **deterministically** and decides who may even ask — an
unauthorised caller is refused before any model is consulted, so the cheap gate
stays cheap. Everything after it is prose, to be weighed by a model against the
reason the caller actually gave.

Policy decides *entitlement*; it never picks machines. What is free, and which of
them can do the work, is the fleet's business — keeping those apart is what lets
policy stay a text file.

**The model half is not wired yet.** Today the controller applies the grants and
the standing limit; the prose is read and carried but not yet reasoned over. That
sequencing is deliberate — the transport, the identity and the refusal paths are
worth proving before anything non-deterministic joins in.

## Try it

Needs two sirjis: one for the organisation, one for a developer.

```sh
# the organisation and the developer pair
SIRJI_HOME=/tmp/acme sirji init && SIRJI_HOME=/tmp/acme sirji daemon &
SIRJI_HOME=/tmp/dev  sirji init && SIRJI_HOME=/tmp/dev  sirji daemon &
INV=$(SIRJI_HOME=/tmp/acme sirji invite dev | tail -1)
SIRJI_HOME=/tmp/dev sirji accept acme "$INV"

# the organisation enrols a controller and a worker
DINV=$(SIRJI_HOME=/tmp/acme sirji device invite cm-c | tail -1)
CM_HOME=/tmp/ctrl cm init --parent "$DINV" --root /tmp/policy
CM_HOME=/tmp/ctrl cm controller &

WINV=$(SIRJI_HOME=/tmp/acme sirji device invite cm-w-1 | tail -1)
CM_HOME=/tmp/w1 cm init --parent "$WINV"
CM_HOME=/tmp/w1 cm worker --slots 1 --can linux &

# the developer enrols a tester, and asks
TINV=$(SIRJI_HOME=/tmp/dev sirji device invite cm-t | tail -1)
CM_HOME=/tmp/tester cm init --parent "$TINV"
CM_HOME=/tmp/tester cm test cm-c@acme "why I need these" --need linux --run "pytest"
```

Every process wants its own `CM_HOME`, because a device may be on another machine.

`scripts/fleet.sh` does all of the above and more — two sirjis, a controller, three
workers with differing capabilities, and a tester walking through every answer the
controller can give:

```sh
SIRJI=/path/to/sirji scripts/fleet.sh
```

## Status

Early, and running end to end: enrolment, resolution, ticket, capability-matched
allocation, real commands dispatched straight to the machines with their output
streamed back live, artifacts returned as bytes, release, and reclaim after a caller
walks away. Verified against a real 1,900-test Playwright suite, sharded across a
fleet and merged into one report.

Next: the model call for `policy.md`'s prose half, and getting the workspace onto a
machine that does not already have it.

## License

Apache-2.0.
