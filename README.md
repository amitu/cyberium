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

## Still `npm test`

The point of a fleet is the suite you already have, run the way people already run
it. Installing `@cyberium/playwright` changes one line of `package.json`:

```jsonc
{ "scripts": { "test": "cm-playwright", "test:here": "playwright test" } }
```

and nothing else. No fixture, no reporter, no import, no spec file touched. `npm test`
now fans out across the fleet; `npm run test:here` is the old command under its own
name, because a tool that silently ran locally when it could not find a fleet would be
indistinguishable from one that had distributed the run:

```sh
$ CM_CONTROLLER=cm-c@acme CM_SHARDS=3 CM_NEED=node20 npm test
machines will fetch 1c2b537b5ade from git@github.com:acme/suite.git
granted 3 machine(s) as r2
[cm-w-1] fetching 1c2b537b5ade… from git@github.com:acme/suite.git
[cm-w-2] fetching 1c2b537b5ade… from git@github.com:acme/suite.git
[cm-w-3] fetching 1c2b537b5ade… from git@github.com:acme/suite.git
[cm-w-1] $ npm ci
  cm-w-1 finished shard 1 with success
  cm-w-2 finished shard 2 with success
  cm-w-3 finished shard 3 with success
merging 3 shard report(s)
  13 passed (2.6s)
```

Playwright has always known how to split a run — `--shard=i/N`, a blob report each,
`merge-reports` at the end. What was missing was somebody to find the machines. Each
shard is an ordinary Playwright process that has no idea it is part of anything,
which is exactly why the suite needs no changes.

**cm has no idea what Playwright is.** It hands out machines and runs commands; the
plugin does the sharding, the blobs and the merge. A `cm` that understood one test
runner would owe the same favour to every other, and the plugin it would have to grow
for each is the one in `plugins/playwright` — about a hundred lines, entirely outside
this repo's Rust.

**A machine starts with nothing.** It does not have your repo, and a fleet that
assumed otherwise would only work on machines somebody had prepared by hand — which
is the same as having no fleet. So each shard fetches the commit you are on, into a
checkout of its own, runs `npm ci`, and has the whole lot deleted when the
reservation ends. Nothing is reused between runs: a working tree left over from
somebody else's job is how a green suite starts depending on what ran before it.

The plugin works out what to fetch from the checkout you are standing in — origin,
`HEAD`, and where you are inside the repo — so the usual case needs no configuration
at all. It warns if it cannot confirm your commit is pushed, and if you have
uncommitted changes: the fleet tests the commit, not your disk.

One thing that will bite otherwise: **shards do not inherit your environment.** A
worker is another machine. Send what the run needs in `CM_ENV`.

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

## Is any of this working?

```sh
$ cm test cm-c@acme --ping
pinging cm-c@acme
  ok    identity               `cm-t` is oli8pqb2gbs1gb4o8ht9athl7ni0uaf0ubv81pd1v31n7rf885j0
  ok    our sirji              reached omahtenu17vh6bgs8t2ahop3f6c5f7pvj6lvnfr1pii250fv6fu0
  ok    resolve                cm-c@acme is 8jjgnu5arbiipum5juidkl3fg5jv9ve1ad7u61153gouc69u1krg
  ok    dial                   ["10.20.1.254:62097", "127.0.0.1:62097"]
  ok    auth                   the controller accepted our ticket
  ok    fleet                  3 machine(s), 3 free, can ["linux", "node20"]
```

Every one of those hops is exercised by a real run too — but a real run reports only
that it failed. These six have six different fixes, so the ping stops at the broken
one and says which:

```
  FAIL  our sirji     … timed out — is the daemon running (`sirji daemon`)?
  FAIL  resolve       we know nobody called "nosuchorg"
  FAIL  resolve       we have no device called "nosuchdevice"
  FAIL  resolve       "cm-c" is not connected
  FAIL  auth          <why the controller turned us away>
```

An empty fleet is reported, not failed: a controller with no machines is working
perfectly, and calling that broken sends people to debug credentials over a fleet
that is merely idle. Pinging takes no machine from anybody.

## Machine hygiene

A machine is lent to one caller after another, so somebody has to be responsible for
what is left between them:

```sh
cm worker --slots 1 --can linux \
  --pre  'scripts/scrub.sh' \
  --post 'scripts/scrub.sh && docker system prune -f'
```

These belong to **whoever runs the machine**, not to whoever borrows it. A caller
cannot supply them, skip them, or read their output — that output is about the
previous tenant, and the point of the scripts is that one tenant learns nothing about
the last.

`--pre` runs when the machine is assigned, before any work is accepted. A caller that
dials during it waits, and is told it is waiting: the controller tells the machine
and the caller about a grant at the same moment, so refusing would punish a caller
for being prompt.

`--post` runs when the reservation ends — released *or* expired, so a caller that
walked away cannot skip it. The worker **leaves the fleet first and cleans up
afterwards**: a machine mid-scrub is not available, and while it stayed registered
the controller could — and in testing did — hand it to somebody new while the last
tenant's cleanup was still running, which defeats the entire point. Dropping the
registration is the existing vocabulary for this, since the connection *is* the
availability. When the scrub finishes the machine offers itself again.

**If cleanup fails the worker stays out and exits non-zero.** A machine whose cleanup
failed may still hold the last tenant's source, credentials or state; being short a
machine is much cheaper than lending that one out. What happens next is for whatever
supervises the process to decide.

Hygiene is machine-wide, so it cannot be combined with `--slots > 1`: a machine
hosting two tenants at once has no moment *between* tenants to clean in, and cleaning
up after one would delete the other's work mid-run. cm refuses that combination
rather than do something plausible and wrong — an operator who wants both wants one
worker per concurrent tenancy.

Neither script is a substitute for the workspace lifecycle: checkouts are already
deleted when the reservation ends. These are for everything cm cannot know about —
containers, caches, browser profiles, stray processes, whatever your machines
accumulate.

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
allocation, machines fetching the code themselves into isolated checkouts, real
commands with their output streamed back live, artifacts returned as bytes, release,
and reclaim after a caller walks away. Verified against a real 1,900-test Playwright
suite, sharded across a fleet and merged into one report.

Next: the model call for `policy.md`'s prose half — the last stubbed part — and
caching the install step, which is now the slowest thing in a run.

## License

Licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE) at your option. © 2026 Amit Upadhyay

Unless you state otherwise, any contribution you intentionally submit for
inclusion in this work shall be dual licensed as above, without any additional
terms or conditions.
