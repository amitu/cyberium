---
title: Quickstart
parent: Guide
nav_order: 2
---

# Quickstart

A controller, a worker and a real test run — on one machine, in about five minutes. Every
command here is one the repository's own scenarios run.

{: .note }
There is a script that does all of this and more: `scripts/fleet.sh` builds a whole fleet,
runs a suite across it, and prints every log at the end. Read it if you would rather see it
than type it.

## 1. An organisation

An organisation is a sirji. Everything under it — controllers, workers, people — is a
**device** of it, and holds no identity of its own.

```sh
export SIRJI_HOME=~/lab/acme
sirji init
sirji daemon &
```

## 2. A controller and a worker

Each is a device, and each gets its own `CM_HOME` because each may be on another machine.

```sh
# on the organisation's sirji: mint an invite per device
INV=$(SIRJI_HOME=~/lab/acme sirji device invite cm-c | tail -1)
CM_HOME=~/lab/cm-c SIRJI_HOME=~/lab/cm-c cm init --parent "$INV" --root ~/lab/cm-c/root

INV=$(SIRJI_HOME=~/lab/acme sirji device invite w1 | tail -1)
CM_HOME=~/lab/w1 SIRJI_HOME=~/lab/w1 cm init --parent "$INV"
```

`--root` is where the controller keeps what it is told: tenants, policies, ledgers.

## 3. Onboard whoever it serves

A controller with no tenants refuses every plea, and says so at startup rather than once
per caller. Tenants exist even self-hosted, where a tenant is usually a team.

```sh
CM_HOME=~/lab/cm-c cm tenant add payments \
    --ceiling 3 --credits 400 --window 86400 \
    --member dana --admin dana --note "the demo team"
```

- `--member` is the caller alias **your own sirji** knows them by. Not a name they claim.
- `--admin` is who may change that team's rules. Absent means nobody.
- `--ceiling` is your cap on them, whatever their policy says.

That writes `tenant.toml` (yours) and a starter `policy.md` (theirs).

## 4. Start it

```sh
CM_MODEL_KEY=$ANTHROPIC_API_KEY \
CM_HOME=~/lab/cm-c SIRJI_HOME=~/lab/cm-c cm controller &

CM_HOME=~/lab/w1 SIRJI_HOME=~/lab/w1 cm worker --can linux --rate 1 &
```

```
controller `cm-c` listening as 514an4oh9vdmfeim2vbnklbub0mk15e19dogeanidbgdqvtrggr0
callers known from: tenants in /Users/you/lab/cm-c/root/tenants
1 tenant(s): payments
policy weighed by: claude-sonnet-5 at https://api.anthropic.com/v1/messages
worker w1 arrived: 1 credit(s)/min, can ["linux"]
```

A worker holds its connection open, and **that connection is its availability**. Kill it and
the controller knows immediately; no heartbeat, no stale roster.

## 5. Ask for machines

The caller is a device of *their own* sirji, not of the organisation's:

```sh
export SIRJI_HOME=~/lab/dana CM_HOME=~/lab/t-dana
sirji init && sirji daemon &
# accept an invite from acme so `cm-c@acme` resolves
INV=$(SIRJI_HOME=~/lab/acme sirji invite dana | tail -1)
sirji accept acme "$INV"
# then enrol a cm device under dana
INV=$(sirji device invite t | tail -1)
cm init --parent "$INV"
```

Check the chain before asking for anything:

```sh
$ cm test cm-c@acme --ping
  ok    identity               dana/t
  ok    resolution             cm-c@acme
  ok    auth                   the controller accepted our ticket
```

`--ping` takes no machine from anybody and names the broken link if there is one.

Then a real run:

```sh
$ cm t cm-c@acme "trying this out" --count 2 --need linux --run 'echo hello from {shard}'
declaring: why=trying this out
granted 2 machine(s) as r1
  (2 machine(s): within what this policy allows)
  expires in 600s unless released
  [w1] hello from 1
  [w2] hello from 2
released r1
```

`cm t` is short for `cm test`. What just happened, in order: your plea was weighed against
`payments`' policy folder by one model call; the answer was clamped to your ask, the team's
ceiling, the budget and what was free; two machines were reserved; the command ran on each
with its own working directory; output streamed back live; and the machines went back.

## What to read next

- [Run a suite](running-tests.html) — sharding, checkouts, artifacts
- [Writing a policy](policy.html) — the part that makes any of this interesting
- [Playwright](playwright.html) — if you have a suite already, start here instead
