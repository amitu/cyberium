---
title: Budgets
parent: Guide
nav_order: 10
---

# Budgets

Allocation without a budget is half an allocator. A ceiling of three machines says nothing
about whether they run for a minute or a fortnight, and the fortnight is what somebody pays
for.

## Credits

**One credit is one minute of the cheapest machine class.** Not a currency: a normalised unit,
so a fleet spanning several countries and two clouds has one number to reason about and no
exchange rate inside it.

Each [worker announces its own rate](workers.html#rates) in credits per minute, because the
machine is what knows what it costs. A grant's worst case is `sum(rates) × lifetime`.

## Setting one

Two places, and the **lower always wins**:

```toml
# tenant.toml — the host's cap
credits = 400
window  = 86400
```

```yaml
# policy.md — the tenant's own, inside whatever the host allows
daily_budget: 200
budget_window: 86400
```

A tenant may hold itself to less than it bought; `daily_budget: 10000` in a policy is a valid
file and changes nothing. And the caller is told which limit bit them, since "you get 1"
without a reason invites somebody to go and edit a policy that was never the constraint.

## Rolling windows, in seconds

```yaml
budget_window: 86400   # rolling, from now
```

No calendar, no timezone, no date. A rolling 24 hours needs no tz database to be correct, and
a billing limit that had to know when a team's day starts would be answering a question it has
no business asking.

Named calendars — *"team_ny follows daylight saving, and their day starts at 08:00"* — are a
thing the prose expresses, with the model naming the rule and deterministic code doing the
arithmetic. Designed, not built; see [the design note](../design/budget.html).

## Commitments count

```
payments   38 of 400 credit(s) used, 120 committed, 242 left
```

A budget that looked only at what had been **spent** would let somebody start a hundred runs
while comfortably under it and find out afterwards. So open grants count at their worst case
until they close.

## Pricing is per machine, in the order they would be picked

Rates 1, 2 and 8 cost 11 credits a minute together, not 3 × the cheapest. An earlier version
priced every machine at the cheapest rate and over-permitted accordingly. So a partial answer
is common and exact:

```
would get 1 machine(s) — 4 allowed, but 39 credit(s) left of 400 buys 1
```

## The model sees the money

The budget, the spend, the commitments and every free machine's rate all go into the prompt.
So a rule like

> If today's budget is more than three quarters spent, hold everything except incidents to the
> standing limit.

works, and the model can answer *"can this wait until tomorrow?"* rather than proposing a
number a clamp then quietly reduces. The deterministic check still runs afterwards — and if it
ever bites, the policy argued past a figure it was shown, which is [logged as a
fault](policy.html#how-the-decision-actually-runs).

## The ledger

One append-only file per tenant, one line per closed reservation, unix seconds:

```
1755620512 3 r1 2 cm-w-1,cm-w-2
1755620530 8 r2 1 cm-w-3
```

Unix time and no date in the filename, because a filename like `2026-08-19.log` bakes a
timezone into a filesystem layout. Spend survives a controller restart — a budget that resets
when a process does is not a budget.

```sh
$ cm admin spend
  payments         38 of 400 credit(s) used, 0 committed, 362 left
  automation       no budget set
```

`no budget set` is a real state, and printing "0 of 0" would read as a tenant that may spend
nothing.

## Not built yet

Currency conversion — `daily_budget: "200 INR"` with the system supplying the rate — is
designed and not built. Today a credit is a credit.
