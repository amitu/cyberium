# policy.md

The fenced block is the only part cm reads for itself. Everything below it is weighed by
a model, along with every other file in this folder.

```yaml
requesters:
  - everyone
standing_limit: 2
max_limit: 6
reservation_seconds: 600
daily_budget: 400
budget_window: 86400
```

## What the pleas are, and who may use which

The pleas we will hear are in `nivedanas/`. Pick the one that describes what you are
actually doing.

Dana experiments constantly and her reasons are never the same twice, so she may only use
pleas from the `noisy-users` folder, and a reason in her own words earns her nothing
beyond the standing limit. Everybody else may name any plea in `nivedanas/routine.md`, or
explain themselves in their own words if none of them fit — though a reason we have not
written down is a weaker case than one we have.

## When a bigger run is justified

A request naming an open production incident may take up to the maximum, but only if it
gives the incident's own identifier in `incident`. An assertion of urgency without one is
not an incident.

Routine work should be counted back towards the standing limit. There is always tomorrow,
and a nightly suite that finishes by morning has lost nothing.

## Money

Prefer the cheapest machines that can do the work. If today's budget is more than three
quarters spent, hold everything except incidents to the standing limit — the last of a
day's credits should be there for something that could not wait.
