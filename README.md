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

## Getting it

Binaries for macOS and Linux are attached to each
[release](https://github.com/amitu/cyberium/releases), with a checksum beside each
one — worth using, since the download crosses a network somebody else administers:

```sh
tag=v0.1.0; target=aarch64-apple-darwin        # or x86_64-apple-darwin,
                                               # x86_64-unknown-linux-gnu,
                                               # aarch64-unknown-linux-gnu
curl -LO "https://github.com/amitu/cyberium/releases/download/$tag/cm-$tag-$target.tar.gz"
curl -LO "https://github.com/amitu/cyberium/releases/download/$tag/cm-$tag-$target.tar.gz.sha256"
shasum -c "cm-$tag-$target.tar.gz.sha256"
tar xzf "cm-$tag-$target.tar.gz"
```

You will want [sirji](https://github.com/amitu/sirji) too — cm is a sirji device,
and every role here needs a sirji to belong to.

**There is no Windows binary yet**, and it is not a matter of adding a target: the
control socket sirji uses is a unix socket, its keystore relies on unix file
permissions, and cm runs commands through `sh`. Windows has answers for all three
(named pipes with an ACL, file ACLs, `cmd`), but the first is a security-model
decision rather than a translation — "filesystem permission is the authorization"
needs its Windows equivalent chosen deliberately.

## How it is put together

Three roles, all sirji **devices**, none holding any identity state:

- **`cm controller`** answers to a name at an organisation's sirji. Anyone that
  organisation has a relationship with can resolve `cm-c@<org>` and reach it. It
  owns the whole picture: which machines are here, what they can do, who has them,
  and when to take them back.
- **`cm worker`** offers **one machine-tenancy** at a time, with a list of
  capabilities and a price. It finds the controller through their shared parent,
  registers, and holds the connection — that connection *is* its availability. No
  heartbeat: QUIC already reports a peer going away. Want concurrency? Run more of
  them, and let the OS supply the limits and isolation it is already good at.
- **`cm test`** is a device of the developer's own sirji. It asks its own sirji to
  resolve the controller, which returns a signed ticket, then dials the controller
  directly and presents it. Granted machines it talks to **directly** — the
  controller allocated, it is not a proxy.

A fourth thing, not a role but a class: an **admin** device, paired by key, which is
the only thing allowed to look at or change how the controller runs. Being one of the
organisation's own devices is not enough — every worker is one of those.

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
cm worker --can linux --can gpu
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
```

Every one of those hops is exercised by a real run too — but a real run reports only
that it failed. These five have five different fixes, so the ping stops at the broken
one and says which:

```
  FAIL  our sirji     … timed out — is the daemon running (`sirji daemon`)?
  FAIL  resolve       we know nobody called "nosuchorg"
  FAIL  resolve       we have no device called "nosuchdevice"
  FAIL  resolve       "cm-c" is not connected
  FAIL  auth          <why the controller turned us away>
```

Notice what it does **not** say: anything about the fleet. What is here is the
controller's business. A summary polled every minute would tell another organisation
your utilisation over time, and from that your release cadence and how often you have
incidents — where a grant only ever tells you about your own request.

What a caller actually wants to know is *what would I get*, and that has its own
answer which takes nothing from anybody:

```sh
$ cm test cm-c@acme "sizing a run" --count 20 --need gpu --dry-run
would get 4 machine(s) — policy allows 20, and 4 matching machine(s) are free right now
```

Same policy, same fleet selection, stopping before anything is held — the selection
code is shared rather than approximated, because an estimate that drifts from the
real path is worse than no estimate. It is a snapshot, not a promise: by the time you
ask for real, the fleet has moved.

## Admins: a third class of device

Tenants ask for machines. Workers offer them. **Admins look at and change how the
controller runs** — the roster, live reservations, tenants, limits, budgets.

An admin is paired **by key**, on the controller, by hand:

```sh
$ cm whoami                      # on the operator's device
cm-ops 00jrbfqkvpvkg1r8e2u78gjvd3t7ehch8hrnbn31n5ud8t4p8k40

$ cm admin add ops 00jrbfqk…     # on the controller
```

Being one of the organisation's own devices is **not** enough, and that distinction
is the point: every worker is one of those, and a machine that offers capacity has no
business reading the roster, every live reservation, or anybody's budget. Membership
is a list the host writes, never something a device acquires by connecting.

```sh
$ cm admin fleet
3 machine(s), 2 free, can ["gpu", "linux"]
  cm-w-1         1 credit(s)/min  can ["linux"]         held by r4
  cm-w-2         2 credit(s)/min  can ["linux"]         idle
  cm-w-3         8 credit(s)/min  can ["linux", "gpu"]  idle

$ cm admin reservations
  r4     dana         1 machine(s), 583s left
```

Two refusals, for two different reasons, and each says which:

```
$ cm admin fleet                                 # from a worker
Error: not an admin of this controller — see `cm admin add`

$ cm admin fleet --controller cm-c@acme          # from another organisation
Error: not an admin of this controller — see `cm admin add`
```

The command deliberately lets anyone ask. Refusing locally would put the decision in
the wrong place and produce a misleading error for a controller that is perfectly
reachable.

Working over sirji rather than a local socket also means it works when the controller
is on a machine the operator cannot log into.

## Machine hygiene

A machine is lent to one caller after another, so somebody has to be responsible for
what is left between them:

```sh
cm worker --can linux \
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

Hygiene is machine-wide, which is safe because **a worker serves one tenancy at a
time.** There is always a moment between tenants to clean in. Concurrency comes from
running more `cm worker` processes, which is also where the operating system's own
limits and isolation live — cgroups, users, containers — rather than something cm
should be reimplementing.

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

The fenced block is read **deterministically** and settles who may even ask — an
unauthorised caller is refused before any model is consulted, so a security decision
never waits on a token. Everything after it is prose, weighed by a model against the
reason the caller actually gave.

Policy decides *entitlement*; it never picks machines. What is free, and which of
them can do the work, is the fleet's business — keeping those apart is what lets
policy stay a text file.

**Every plea is weighed against the prose** — one model call, and the number it
returns *is* the allocation. Not a fast path that answers the easy ones and a model
for the rest: how many machines a request deserves is the question the policy was
written to answer, and a number arrived at without reading it would be a default
wearing the organisation's name.

```yaml
standing_limit: 2      # the answer when the prose *cannot* be weighed
max_limit: 4           # the most any interpretation may grant. Absent means it
                       # may lower a number but never raise one.
```

Both are bounds on the answer, not stages before it. `max_limit` is a ceiling a
human wrote and the model never exceeds; `standing_limit` is what this organisation
stands behind with nothing interpreted — which is exactly the right answer when
nothing can be, and nothing more than that:

- **The answer is clamped** to `max_limit` and to what was actually asked for. Told
  to hand out 99 against a `max_limit` of 4, it hands out 4. The model argues; it
  never becomes the gate.
- **A refusal is always honoured.** Clamping is one-directional, and down is safe.
- **Unreachable is not permission.** No key, a timeout, an API having a bad day —
  the standing limit answers and says so in the rationale. A fleet that stopped
  working because a model was down would be worse than one with no prose at all.
  With no key at all `cm` still allocates; it just says the prose went unread.

Only one thing is decided before the prose: whether this caller may ask at all.
That needs no interpretation, so an unauthorised caller is refused without spending
a token.

The model never sees fleet state. Mixing entitlement with availability would make
the same plea weigh differently depending on who else happened to be running, and
an unreproducible decision cannot be tested — so a scenario greps the assembled
prompt for fleet vocabulary and fails if it finds any. Both numbers are logged,
what the model said and what it was bounded to, because "why did I get 4" has to
be answerable.

```sh
CM_MODEL_KEY=…   # unset means no model, and the controller still works
```

Because the prompt carries no fleet state, a tenant's policy prompt is byte-identical
from one plea to the next, so it is sent marked cacheable. The property that keeps
decisions reproducible is the same one that makes them cheap to repeat.

**A folder per tenant, and two files with different owners:**

```sh
$ cm tenant add payments --ceiling 3 --member dana --member kiran
tenant `payments` at …/tenants/payments
  ceiling 3 machine(s)
  members  dana, kiran
  they edit …/tenants/payments/policy.md
  you own  …/tenants/payments/tenant.toml
```

**Always tenants — self-hosted too**, where a tenant is usually a *team* rather than
an organisation. One model either way, and one place spend is counted. With no
`--member`, a tenant's own name is its only member, which is the common case.

A tenant writes `policy.md`; the host writes `tenant.toml`. Without that split, an
organisation authoring its own policy would be authoring its own quota —
`standing_limit: 10000` is a valid file. The lower of the two wins, and the caller is
told *which* limit bit them, so nobody edits a policy that was never the constraint.

The tenant is chosen by **the verified alias in the caller's ticket** — minted by the
controller's own sirji, not asserted by the caller — so this needed no accounts and
no new credential. Adding a tenant or editing a policy needs no restart.

What is not built: `nivedanas/`, the model call, `cm test-policy`, `cm upload-policy`,
and the credential story. Three design notes carry those, each opening with what does
and does not exist: [docs/policy.md](docs/policy.md), [docs/budget.md](docs/budget.md)
and [docs/auth.md](docs/auth.md).

## Budgets

A ceiling of three machines says nothing about whether they ran for a minute or a
fortnight, and the fortnight is what somebody pays for.

```sh
$ cm worker --can linux --rate 1          # each machine announces
$ cm worker --can linux --can gpu --rate 10   # what it costs

$ cm tenant add team --credits 40 --window 3600 --member dana
$ cm admin spend
  team             34 of 40 credit(s) used, 0 committed, 6 left
```

The unit is **a credit** — one minute of the cheapest machine class — and money never
enters the fleet, because a GPU box and a small Linux box are not comparable in dollars
across regions and contracts, while *"that one costs ten of these"* is true everywhere.

Consequences worth knowing, each of which was a decision:

- **Selection is cheapest first.** Without it, a cost-aware allocator was accidentally
  indifferent to price — `--need linux` could spend the GPU box's rate while an
  ordinary machine sat idle.
- **Commitments count, not just spend.** Otherwise a tenant starts a hundred runs at
  once while comfortably under budget and finds out afterwards.
- **Machines are priced individually.** At rates 1, 2 and 8 a three-machine grant
  costs 11 a minute, not 3.
- **The refusal says which limit bit.** `budget spent: 34 of 40 credit(s) used or
  committed in the last 3600s` — and a partial answer says what the remainder buys.
- **Unix time only.** No timezone in configuration, no date in a filename, because
  what counts as a day is policy and policy changes. Windows are rolling seconds.

The ledger is one append-only file per tenant, which is what makes "why is our budget
gone" answerable — a running total never is:

```
1787126227 1  r1 1 cheap
1787126231 11 r2 1 cheap,dear
```

Not built: currency conversion (`daily_budget: "200 INR"`), and the named calendars
a team will want (*"our day starts at 08:00 New York, daylight saving included"*) —
both need the model, and the split is settled in [docs/budget.md](docs/budget.md):
the model names the rule, deterministic code does the calendar arithmetic against a
real tz database, because DST is not a thing to trust a language model with.

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
CM_HOME=/tmp/w1 cm worker --can linux &

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
reclaim after a caller walks away, per-tenant policy, credit budgets, and the model
call for `policy.md`'s prose half. Verified against a real 1,900-test Playwright
suite, sharded across a fleet and merged into one report.

Next: `nivedanas/` — named pleas, so a policy is weighed against a phrase the
organisation wrote rather than free text a caller typed — then `cm test-policy`, so
a policy change can be reviewed like code. And caching the install step, which is
now the slowest thing in a run.

## License

Licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE) at your option. © 2026 Amit Upadhyay

Unless you state otherwise, any contribution you intentionally submit for
inclusion in this work shall be dual licensed as above, without any additional
terms or conditions.
