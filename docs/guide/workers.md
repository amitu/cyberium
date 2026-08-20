---
title: Workers
parent: Guide
nav_order: 5
---

# Workers

```sh
cm worker [--controller <name>] [--can <cap>]... [--rate N] [--pre <cmd>] [--post <cmd>]
```

A worker offers one machine. It finds the controller through their shared parent, registers,
and holds the connection open — and **that connection is its availability**. No heartbeat, no
timeout arithmetic, no roster that can go stale: kill a worker and the controller knows
immediately, because the socket closed.

## Capabilities

```sh
cm worker --can linux --can node20 --can has-2fa-sim
```

Plain strings, and the organisation invents its own vocabulary. Nothing in cyberium
understands `has-2fa-sim` and nothing needs to in order to match on it. A plea's `--need` is
matched against these; a machine missing any of them is not a candidate.

## Rates

```sh
cm worker --can linux --rate 8
```

What this machine costs in **credits per minute while held**. The machine announces its own,
because the machine is what knows — a GPU box and a spare laptop should not be described by
the same number in someone else's config file.

Unstated means 1, never free. See [Budgets](budgets.html) for what a credit is.

Allocation is **cheapest-first**: a plea for `linux` gets the cheap machines while they last,
not whichever came up first. Without that, a cost-aware allocator is accidentally indifferent
to cost, which is arguably the single most valuable line in the whole feature.

## One tenancy at a time

A worker serves one reservation. There is no `--slots`:

> Run more `cm worker` processes if you want concurrency, and let the operating system
> provide the limits and the isolation it is already good at.

Slots made a rate ambiguous — whose minute is this, when two tenants share a machine? — and
they made the cleanup scripts below impossible to honour, because you cannot scrub a machine
between tenants while another tenant is still on it.

## Cleaning up between tenants

```sh
cm worker --can linux \
  --pre  'docker system prune -f && rm -rf /tmp/work/*' \
  --post 'pkill -u runner || true; rm -rf /home/runner/.cache/app'
```

`--pre` makes the machine fit before each tenancy. `--post` takes back what the last tenant
left.

Both belong to whoever runs the machine. **A caller cannot supply them, skip them, or see
their output** — they are the machine owner's guarantee to the next tenant, not part of
anybody's job.

**A `--post` that fails takes the machine out of the fleet** rather than lending out a dirty
one. If cleanup did not work, the honest thing is to stop offering the machine.

{: .note }
Cleanup runs *after the machine has left the fleet* and before it returns. An earlier version
ran `--post` while still registered, and the controller handed the machine to a new tenant
mid-scrub. Order matters more than it looks.

## What a worker does with a plea

Nothing, until told. Then, per shard:

1. Fetches `--repo` at `--ref` into a **fresh checkout** — one per shard, never reused.
2. Runs `--setup` once.
3. Runs `--run`, streaming stdout and stderr back live rather than buffering to the end.
4. Sends back whatever `--collect` names, as bytes.
5. Deletes the checkout when the reservation ends.

Workers never speak to each other. They have nothing to say to each other: everything that
needs a view of the whole fleet lives in exactly one place, and that place is the controller.

## Admission

A worker refuses work for a reservation it was never told about. The controller signs a
ticket admitting a caller to a machine, and the machine verifies it against the controller
key it registered with — the same mechanism sirji uses one level up, reused one level down.

So a caller who learns a worker's address gains nothing by dialling it, which is why a grant
can safely hand out addresses at all.
