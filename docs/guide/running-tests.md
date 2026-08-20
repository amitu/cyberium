---
title: Run a suite
parent: Guide
nav_order: 3
---

# Run a suite

```sh
cm t <name@org> ["why"] [--count N] [--need <cap>]... [--run <cmd>] [more]
```

Two arguments are cyberium's own, because it acts on them mechanically rather than
interpreting them:

**`--count N`** bounds the grant. Nobody is handed machines they did not ask for, so this is
a ceiling and never a request to be topped up.

**`--need <cap>`** picks machines that can do the work, matched against what each worker
declared. Repeatable, and every one must match. A box without `gpu` cannot run gpu tests
and no policy changes that.

Everything else you pass is for your own policy to read. See
[declarations](#saying-things-to-your-own-policy).

## Sharding

`{shard}`, `{index}` and `{shards}` are substituted in `--run`, `--env` and `--collect`.

```sh
cm t cm-c@acme --plea nightly --count 4 \
    --run 'npx playwright test --shard={shard}/{shards}'
```

`{shard}` is 1-based, `{index}` 0-based, `{shards}` the total **actually granted** — not what
you asked for. That matters: if you ask for six and a policy counters at three, the split is
a correct three-way split rather than half of a six-way one.

## Getting the code onto the machines

Workers fetch it themselves. They never receive a copy from you, and they never reuse a
checkout:

```sh
cm t cm-c@acme --plea nightly --count 4 \
    --repo https://github.com/acme/suite --ref "$(git rev-parse HEAD)" \
    --dir packages/api --setup 'npm ci' \
    --run 'npx playwright test --shard={shard}/{shards}'
```

| Flag | What it does |
|---|---|
| `--repo <url>` | fetched before anything runs |
| `--ref <commit>` | which commit — pass a SHA, not a branch, or you are testing a moving target |
| `--dir <subdir>` | run below the repo root |
| `--setup <cmd>` | once, before the run: `npm ci`, `bundle install` |
| `--cwd <dir>` | run here instead, when the machine already has the code |

**Each shard gets its own checkout**, deleted when the reservation ends. Two shards of the
same suite on one machine cannot see each other's files, and nothing survives to be
inherited by the next tenant.

## Bringing things back

```sh
cm t … --collect 'blob-report/shard-{shard}.zip' --artifacts ./reports
```

`--collect` is repeatable and takes the path *on the machine*. Files come back as bytes over
the same connection and land under `--artifacts` in a directory per shard.

{: .note }
Only the filename of a returned artifact is trusted. A worker answering with
`../../../etc/passwd` writes a file called `passwd` inside your artifacts directory, and
there is a test for exactly that. The path came from another machine; only its shape is
yours to trust.

## Asking without taking

```sh
$ cm t cm-c@acme --plea nightly --count 6 --need linux --dry-run
declaring: plea=nightly
would get 2 machine(s) — a nightly suite has all night, so this is held to the standing limit
```

`--dry-run` runs **exactly** the decision a real plea runs — the same policy, the same model
call, the same machine selection — and stops before reserving anything. It shares the
selection code with the real path rather than estimating, because an approximation would
eventually disagree with reality and be believed.

Nobody's run is disturbed to answer it, which is what makes it usable on a busy fleet.

## Saying things to your own policy

Any other `--key value` is passed through untouched. cyberium attaches no meaning to any of
it:

```sh
cm t cm-c@acme --count 6 --need linux \
    --plea production-incident --incident INC-4471 --urgent
```

```
declaring: incident=INC-4471 plea=production-incident urgent=true
```

`--plea` is not a feature. Neither is `--incident`. A bare `--key` becomes `key=true`. What
each is worth is written in [your policy](policy.html) — which is the point: your vocabulary
does not need cyberium's permission to exist.

Every declaration is echoed, because an unknown flag becomes a declaration rather than an
error. A mistyped `--dry-runn` is then a harmless key instead of a refusal, and the echo is
how you notice it did not dry-run.

From CI, where adding arguments is awkward:

```sh
CM_SAY='plea=nightly-regression,incident=INC-4471'
```

## When it says no

```
countered: 2 — a nightly suite has all night
```
You may have fewer. The rationale is the policy's own words about your own request.

```
denied: 1 machine(s) here can do ["gpu"], and none are free right now — worth retrying
```
Nothing now. Note that this is deliberately different from *"no machine in the fleet can do
gpu — waiting will not change that"*: one means retry, the other means give up, and a caller
with retry logic needs to know which.

```
the controller could not weigh this request: calling the model: …
  nothing was refused, and nothing was allocated. Tell whoever runs it.
```
Nobody read your request. Not a refusal — do not go and argue with a policy that was never
consulted. This exits non-zero, and the fix belongs to whoever runs the controller.

## If your process dies

Grants expire. The reservation lifetime comes from the tenant's own policy
(`reservation_seconds`), and a caller that walks away holding machines has them taken back:

```
dana's r5 expired unreleased, taking back 1 machine(s) — 8 credit(s)
```

You are still billed for what you held. `--abandon` exists to watch this happen on purpose;
nothing a real caller wants, and everything a crashed one does by accident.
