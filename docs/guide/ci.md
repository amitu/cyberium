---
title: Running from CI
parent: Guide
nav_order: 5
---

# Running from CI

A cloud CI runner exists for ninety seconds and has nothing to enrol. So it does not
enrol: it mints a keypair for the run, gets a token whose **audience is that key**, dials
the controller, and throws the key away.

No `cm init`. No shared secret. No entry in anybody's roster — a runner that enrolled would
leave one dead entry per build.

```yaml
jobs:
  test:
    permissions:
      id-token: write          # without this there is no token to get
    steps:
      - uses: actions/checkout@v4
      - run: npm test
        env:
          CM_CONTROLLER: ${{ vars.CM_CONTROLLER_KEY }}
          CM_CONTROLLER_HINTS: ${{ vars.CM_CONTROLLER_HINTS }}
          CM_SAY: plea=pre-merge-check,pr=${{ github.event.number }}
```

`id-token: write` is the whole of the setup on the CI side. GitHub then exposes an endpoint
that mints a token per audience, and `cm` asks it for one naming this run's key.

## Why a scraped token is worthless

This is the property everything else rests on, so it is worth being explicit.

cm **does not accept bearer tokens.** The token's audience must be the public key the
caller is dialling from, and that key was generated at the start of the run and never
written to disk. A token lifted out of a build log names an audience whose private key no
longer exists anywhere.

It is the same caller-binding a sealed ticket has, from a credential cm did not issue. An
issuer that cannot set the audience cannot be used this way, and that is the correct
outcome rather than a gap to work around.

## What the host configures

Two things, both host-owned, because both decide whose word counts as proof.

**`<root>/issuers.toml`** — who this controller believes:

```toml
[[issuer]]
name    = "github"
url     = "https://token.actions.githubusercontent.com"
jwks    = "https://token.actions.githubusercontent.com/.well-known/jwks"
subject = "repository"
allow   = ["acme/*"]
facts   = ["ref", "event_name", "workflow", "actor"]
```

| Field | Meaning |
|---|---|
| `name` | prefixes every alias it vouches for: `github:acme/payments` |
| `url` | the `iss` claim, matched **exactly** |
| `jwks` | where the signing keys are published |
| `subject` | which claim names the caller |
| `allow` | patterns the subject must match. `*` at either end |
| `facts` | other claims to carry through as attested facts |

A missing file means **nobody**. `allow = []` is refused at load, because an issuer that can
vouch for nobody is a configuration mistake that would surface as an unexplained refusal.

`url` is compared by equality, not prefix: `token.actions.githubusercontent.com.evil.test`
starts with the real thing.

**The tenant that repository belongs to** — the alias is `<issuer>:<subject>`, and the
prefix is why it cannot collide with a sirji alias:

```sh
cm tenant add ci --ceiling 8 \
    --member github:acme/payments \
    --member github:acme/payments-api
```

## The claims become facts a policy can use

Everything in `facts` arrives in the **attested** half of the prompt, beside the caller's
identity, where the caller cannot touch it:

```
ATTESTED (proven — the requester cannot influence any of this)
tenant: ci
caller: github:acme/payments
event_name: pull_request
issuer: github
ref: refs/pull/41/merge
repository: acme/payments
```

So a policy can say things it could not otherwise trust:

```markdown
A pull request build may have up to six machines while somebody is waiting on it. A
scheduled run is routine and waits — `event_name` says which this is, and it comes from
the platform rather than from the job, so a nightly cannot describe itself as a PR.
```

That last clause is the point of keeping attested and declared apart. A request declaring
itself urgent while attested as a scheduled run has told you something, and only a system
that kept them separate can notice.

## Any other provider

`CM_ATTEST_CMD` is a command that prints a token, with `{audience}` replaced by this run's
key:

```sh
CM_ATTEST_CMD='my-idp-token --audience {audience}'
```

One hook rather than an integration per provider. Note what is deliberately **not** offered:
a variable holding a ready-made token. It could never be right — the audience has to be a
key that does not exist until the run starts, so a token prepared in advance is either for
the wrong audience or is a bearer token.

Nothing about the token is checked locally. The controller is the only party whose opinion
of it matters, and a client that pre-validated would be a second implementation to disagree
with the first.

## Addressing the controller

An attested caller has no parent to resolve through, so it dials the key directly:

```sh
cm t <controller-id52> --plea pre-merge-check --count 4 --need linux
```

The controller prints both at startup:

```
controller `cm-c` listening as 04215g0sc805rp6qj6cicckdtsaku7fbeaaphe65oedrfeb0a5bg
reachable at: 10.20.2.196:59000, 127.0.0.1:59000
attestations accepted from: github (acme/*)
```

Put the key in `CM_CONTROLLER` and, if discovery is unreliable on your network, the address
in `CM_CONTROLLER_HINTS`. DNS-based discovery of a handshake key is designed and not built.

A bare word that is neither `name@org` nor an id52 is refused with both forms named, rather
than failing later on a decode error.

## What an attested caller cannot do

**Be an admin.** Admin membership is by key, from a list a host wrote by hand, and this
caller's key was minted for one run. No claim in a token changes that, and there is a test
for it.

**Be a sibling.** Attestation makes you a known caller, not one of the organisation's own
devices.

**Raise its own limits.** It is weighed against its tenant's policy like anybody else, and
the ceiling, the budget and availability all still apply.

## Trying it without a CI system

`scripts/attest.sh` runs the whole thing locally: an RSA key, a JWKS endpoint served over
HTTP, real signatures, and a caller with no `CM_HOME` at all. It also demonstrates each
refusal — a token minted for somebody else's key, a repository outside `allow`, a tampered
signature, an unknown issuer.
