---
title: Operating a controller
parent: Guide
nav_order: 11
---

# Operating a controller

## Admin devices

Looking inside a controller is a privilege, and being one of its own devices is not enough — a
worker is one of those.

```sh
# on the machine that will do the looking
$ cm whoami
ops 87msg2kfkl8k6paa03ipmj728gan3j0837npihjlua2fcd8g77ig

# on the controller itself
$ cm admin add ops 87msg2kfkl8k6paa03ipmj728gan3j0837npihjlua2fcd8g77ig --note "the operator laptop"
$ cm admin list
```

`cm admin add` is local on purpose. This decides who may change how the controller runs, so it
is the host's act at the host's keyboard — and the first admin has to be added this way. There
is no bootstrap in which a device grants itself the power to grant power.

Membership is **by key**, not by being a sibling. The list is read at startup and not re-read:
adding an admin is rare, deliberate, and worth a restart, and a list that reloads itself is a
list a stray file write can extend.

## Looking inside

```sh
cm admin fleet [--controller <name>]
cm admin reservations [--controller <name>]
cm admin spend [--controller <name>]
```

```
$ cm admin fleet
  cm-w-1         1 credit(s)/min  can ["linux"]  idle
  cm-w-2         2 credit(s)/min  can ["linux"]  held by r3
  cm-w-3         8 credit(s)/min  can ["linux", "gpu"]  idle

$ cm admin reservations
  r3  dana (payments)  1 machine(s)  8 credit(s)/min  expires in 412s

$ cm admin spend
  !! automation: configuration not read, still running on the last good copy — …
  payments         38 of 400 credit(s) used, 120 committed, 242 left
  automation       no budget set
```

Trouble is printed first, because it changes how to read everything under it: a tenant listed
that way is running on terms that are not the ones in its file, and nothing else about the
fleet looks wrong.

## What callers never learn

A ping answers "we are here, and we accepted your ticket" and **nothing else**. It used to
carry a fleet summary, justified as disclosing nothing a grant would not — which was wrong. A
grant tells you about *your* request. A summary polled every minute tells another organisation
your utilisation over time, and from that your release cadence, your team's size, and how often
you have incidents.

What a caller actually needs is answered better by asking, and `--dry-run` answers it without
taking anything.

## Reading the log

```
policy weighed dana: said 4, giving 4 — an incident with a tracker reference
policy refused kiran: a routine regression run has all night
policy for payments overshot limits it was shown (proposed 99 against a stated
  ceiling of 6) — cut to 6. Fix the policy; the prompt stated every one of them.
could not weigh policy for dana: calling the model: error sending request …
dana's r5 expired unreleased, taking back 1 machine(s) — 8 credit(s)
ops replaced payments's policy: 3 file(s) — nivedanas/routine.md, policy.md
```

Both numbers are always logged — what the model said and what it was given — because "why did I
get four" has to be answerable, and a clamp that left no trace would make your own ceiling
invisible.

An **overshot** line is a defect report about a policy, not routine noise. Every limit it names
was in the prompt.

## Restarts

Tenants and policies are re-read on demand, so adding a tenant or editing a policy needs no
restart. Spend survives a restart, because it is on disk. Live reservations do **not**: a
controller that comes back has an empty fleet, and workers re-register as they reconnect.
