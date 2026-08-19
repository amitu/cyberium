# Budgets, and the unit they are counted in

> **Status: designed, not built.** The unit, where rates are declared, and how a
> budget clamps a grant are settled here. No code reads any of it yet.

Allocation without a budget is only half an allocator. A ceiling of three machines
says nothing about whether those three ran for a minute or a fortnight, and the
fortnight is what somebody pays for.

## The unit

**A credit.** One credit is the cost of running the cheapest machine class in the
fleet for one minute, and the host decides what "cheapest" means. Everything else is
priced relative to that.

Why an abstract unit rather than money:

- **Machines differ by more than price.** A GPU box and a small Linux box are not
  comparable in dollars-per-hour across regions, contracts and spot markets, but a
  host can say "that one costs eight of these" and be right everywhere.
- **Currency is somebody else's problem.** Infrastructure should not carry exchange
  rates. A fleet in three countries has one number per machine, not three.
- **Prices change; policies should not.** A supplier renegotiation moves rates. It
  must not require every tenant to rewrite `policy.md`.

The obvious risk is that people will read "credit" as money and ask what one is
worth in dollars. That question has an answer, and it belongs to the host — which is
exactly why the unit is not denominated in any currency.

Names considered and rejected: **unit** (says nothing), **point** (sounds like a
reward scheme), **mudra** — which fits the protocol's Sanskrit vocabulary well, but
`policy.md` is written by customers who are not cm insiders, and the CLI and config
have deliberately stayed plain English while only the wire protocol is Sanskrit.

## Where rates come from

**A worker announces its own rate.** It is the thing that knows what it is:

```sh
cm worker --slots 1 --can linux --rate 1
cm worker --slots 2 --can linux --can gpu --rate 8
```

Credits per minute, per slot in use. A worker that says nothing costs the default of
one, because a machine of unknown cost must not be free.

Announced rather than configured centrally for the same reason capabilities are: the
machine knows, and a second list to keep in step is a second list to get wrong.

## What a grant costs

```
cost = Σ over granted machines (rate) × minutes held
```

Charged on **release or expiry**, against the reservation's tenant, for the time
actually held — not the time asked for. A caller who releases early pays less, which
is the incentive the design already wants: releasing promptly is how capacity comes
back.

The expiry backstop matters here too. A caller who walks away pays for the full
reservation lifetime, because that is what they cost the fleet.

## Two budgets, like the two ceilings

The same split, for the same reason:

| limit | set by | in |
|---|---|---|
| `daily_credits` | the host, in `tenant.toml` | credits |
| `daily_budget` | the tenant, in `policy.md` | credits, or a currency |

A tenant divides what they have between teams and purposes; the host decides how
much that is. Without the outer one, `daily_budget: 100000000` is a valid file.

## Currency in policy.md, credits everywhere else

A policy author should be able to write what their finance team told them:

```yaml
daily_budget: "200 INR"
```

so the controller supplies the conversion and does the arithmetic. Rates live with
the host, beside the definition of a credit:

```toml
# <root>/rates.toml — the host's, never a tenant's
credit_in = { INR = 2.5, USD = 0.03, EUR = 0.028 }
```

Two rules that keep this honest:

- **Convert at evaluation, and record both.** A decision log saying "spent 4000
  credits" is unauditable a month later if the rate has moved; it must say "4000
  credits, which was 10000 INR at 2.5".
- **A missing rate is an error, not a zero.** A budget in a currency the host has no
  rate for must refuse loudly. Silently treating it as unlimited is the worst
  available outcome.

Prose may also mention money — *"a production incident may spend up to 5000 rupees"*
— and the model needs the same conversion table in its prompt to reason about it.
That is org-authored text, so it carries the same trust as the rest of `policy.md`.

## What has to be persistent

A daily budget is meaningless if it resets when the controller restarts. So spend is
a **ledger on disk**, per tenant, appended as reservations close:

```text
<root>/tenants/dana/spend/2026-08-19.log
```

Append-only, one line per closed reservation, with the reservation id, the machines,
the minutes, the rate applied, and the resulting credits. That makes "why is our
budget gone" answerable, which a running total never is.

Deliberately not a database. An operator should be able to read it, and a day's
worth of allocations is small.

## Open questions

- **Which day?** A budget "per day" needs a timezone, and the tenant's is not the
  host's. Probably declared per tenant, defaulting to UTC.
- **What happens at the limit** — refuse, or counter with fewer machines and a
  shorter lifetime? Countering is kinder and more complicated, and the model is well
  placed to choose. It should probably be policy's decision, not the code's.
- **Reserved versus spent.** A long reservation's cost is unknown until it closes, so
  a tenant near its limit could start a run it cannot afford. Charging the maximum up
  front and refunding on release is the honest fix, and it makes the arithmetic
  harder to explain.
- **Who sees spend.** An admin, certainly. A tenant should see their own and nobody
  else's, which needs a caller-facing view that discloses only their own numbers —
  the same discipline that removed the fleet summary from a ping.
