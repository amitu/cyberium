# Budgets, and the unit they are counted in

> **Status: built**, in its deterministic form. Worker rates, cost on close, the
> per-tenant ledger, the two rolling-window budgets and the clamp all run. What is
> **not** built: currency conversion, prose budgets (they need the model), and the
> named-calendar windows described below — the rolling window is what works today.

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

Credits per minute while held. A worker that says nothing costs one, because a machine
of unknown cost must not be free.

And selection is **cheapest first**, which this made necessary rather than merely
nice: picking in name order left a cost-aware allocator accidentally indifferent to
price, so `--need linux` could spend a GPU box's rate while an ordinary machine sat
idle.

Announced rather than configured centrally for the same reason capabilities are: the
machine knows, and a second list to keep in step is a second list to get wrong.

## What a grant costs

```
cost = Σ over granted machines (rate) × minutes held
```

Charged on **release or expiry**, against the reservation's **tenant** — not the
caller, since several callers may share one — for the time actually held rather than
the time asked for. A caller who releases early pays less, which is the incentive the
design already wanted: releasing promptly is how capacity comes back. A caller who
walks away pays for the full lifetime, because that is what they cost the fleet.

Part-minutes count as a minute. A fifty-second run costing nothing would make the
fleet free to anybody willing to churn.

The rate is **fixed when the grant is made**, not looked up at close: a machine may
have departed by then, and in any case you pay what was agreed when you took it.

**Commitments count against the budget, not just spend.** A budget that looked only at
what had been spent would let a tenant start a hundred runs at once while comfortably
under it and find out afterwards. So the check is `budget − spent − worst-case of open
reservations`, and a grant is allowed only if it would still fit when nobody releases.

Machines are priced **individually, in the order they would be taken**. At rates 1, 2
and 8 a three-machine grant costs 11 a minute, not 3 — pricing them all at the
cheapest was a bug this design had for about an hour, and a budget that over-permits
is one that discovers the overspend afterwards.

## Two budgets, like the two ceilings

The same split, for the same reason:

| limit | set by | in |
|---|---|---|
| `credits` + `window` | the host, in `tenant.toml` | credits over a rolling number of seconds |
| `daily_budget` | the tenant, in `policy.md` | credits or a currency, over a rolling window or a named calendar |

A tenant divides what they have between teams and purposes; the host decides how
much that is. Without the outer one, `daily_budget: 100000000` is a valid file.

The host's limit is deliberately the **duller** of the two: a rolling window of
seconds, with no calendar and so no timezone. It is a billing ceiling, and a billing
ceiling that needed to know when a team's day starts would be answering a question it
has no business asking. Calendars belong to the tenant, in prose, where they can be
as particular as they like.

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

## Unix time, and nothing else

**cm stores instants. It never stores days.**

Every timestamp in the ledger is unix seconds. There is no timezone in cm's
configuration, no date in a filename, and no bucketing at write time — because *what
counts as a day is policy*, and policy changes. A ledger that had already been
bucketed could not be re-read under a new rule; one made of instants can.

That also keeps cm out of an argument it has no standing in. A day starts at 8am in
New York for one team and at midnight UTC for another, one of them observes daylight
saving and the other does not, and a third counts a fiscal month. None of that is
infrastructure's business.

### Who does which half

Timezones are semantic, so prose handles them:

```markdown
team_ny works New York hours, daylight saving included, and their day starts at 08:00.
The platform team is on UTC. Neither may spend more than 4000 credits in a day.
```

But **the model must not do calendar arithmetic.** "Is unix 1786968121 after 08:00 in
New York today, given DST" is exactly the sort of question a model answers
confidently and wrongly. So the work splits along the line the rest of the design
already uses:

| does | who |
|---|---|
| read the prose and name the rule | the model — `{ tz: "America/New_York", day_starts_at: "08:00" }` |
| turn that into a unix range | deterministic code, against a real tz database |
| sum the ledger over that range | deterministic code |

**The model extracts structure; code does the arithmetic.** DST correctness then comes
from the tz database rather than from a language model's confidence, which is the only
version of this that is safe to bill against.

Cost: a tz database dependency, once the model half exists. Worth it, and narrower
than the alternative — a schema of per-team timezone fields, DST flags and fiscal
calendars that would never stop growing.

### The deterministic half needs no calendar at all

The cheap gate cannot wait for a model, so budgets in the fenced block are a
**rolling window in seconds**:

```yaml
daily_budget: 4000
budget_window: 86400        # rolling, from now
```

A rolling 24 hours needs no timezone, no DST and no tz database — which is why the
deterministic path can enforce a budget today and stay correct. A named calendar is
what you graduate to when a team needs their day to start at 08:00.

### The ledger

One append-only file per tenant, one line per closed reservation:

```text
<root>/tenants/dana/spend.log
  1786968121 r7 cm-w-1,cm-w-3 12m rate=9 credits=108
```

Read backwards until the timestamps leave the window, which serves both a rolling
window and a named one. Deliberately not a database: an operator should be able to
read it, and it is what makes "why is our budget gone" answerable — a running total
never is.

Sharding, if it is ever needed, must be by **size or count, never by date**. A date
in a filename is a timezone decision smuggled into a filesystem layout.

## Open questions

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
