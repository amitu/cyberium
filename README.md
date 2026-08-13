# cm

Cost-aware allocation of test machines, over [sirji](https://github.com/amitu/sirji).

A developer asks for machines in English, and a controller weighs that plea
against the organisation's own written policy. **Allocation is a negotiation, not
a booking** — a request may be granted, countered with a smaller shape, or
refused with a reason you can act on.

```
$ cm test cm-c@acme "running the checkout suite before merge" --count 3
resolved cm-c@acme -> tao42kdq4lv3v5lqfkcb473affiao57qcnft00l7v978ee8kup90
granted 3 worker(s)
  cm-w-0
  cm-w-1
  cm-w-2
(within the standing limit of 10)
```

## How it is put together

Both roles are sirji **devices**, and neither holds any identity state:

- **`cm controller`** answers to a name at an organisation's sirji. Anyone that
  organisation has a relationship with can resolve `cm-c@<org>` and reach it.
- **`cm test`** is a device of the developer's own sirji. It asks its own sirji to
  resolve the controller, which returns a signed ticket, then dials the controller
  directly and presents it.

The controller learns who is asking from that ticket alone — it has no
`network.toml`, has never heard of the caller, and cannot look anything up. It
verifies one signature from its own parent. The developer, symmetrically, learns
nothing about the organisation's internals.

There is no shared secret anywhere, no API key, and no account. Identity is an
ed25519 keypair, connections are QUIC, and the substrate handles all of it.

## policy.md

One file, hand-edited, `git`-able. Two halves on purpose:

```markdown
```yaml
requesters:
  - everyone
standing_limit: 10
```

## Circumstantial override

If a request asserts a production incident and names an incident tracker URL,
allow up to 5x the standing limit for one hour, then re-evaluate.
```

The fenced block is read **deterministically** and decides who may even ask — an
unauthorised caller is refused before any model is consulted, so the cheap gate
stays cheap. Everything after it is prose, to be weighed by a model against the
reason the caller actually gave.

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
INV=$(SIRJI_HOME=/tmp/acme sirji invite dev)
SIRJI_HOME=/tmp/dev sirji accept acme "$INV"

# the organisation enrols a controller
DINV=$(SIRJI_HOME=/tmp/acme sirji device invite cm-c)
CM_HOME=/tmp/ctrl cm init --parent "$DINV" --root /tmp/policy
CM_HOME=/tmp/ctrl cm controller &

# the developer enrols a tester, and asks
TINV=$(SIRJI_HOME=/tmp/dev sirji device invite cm-t)
CM_HOME=/tmp/tester cm init --parent "$TINV"
CM_HOME=/tmp/tester cm test cm-c@acme "why I need these" --count 3
```

## Status

Early. The controller answers, the tester asks, and the whole path — enrolment,
resolution, ticket, refusal — runs end to end. Workers are placeholders, and the
model call is the next thing.

## License

Apache-2.0.
