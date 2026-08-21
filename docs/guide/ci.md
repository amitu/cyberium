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
          CM_CONTROLLER: cm.acme.com     # a name, not a key — see below
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

Three ways, and a CI variable should hold the second.

```sh
cm t cm-c@acme          # from an enrolled device: your own sirji resolves it
cm t cm.acme.com        # a host that publishes which controller it runs
cm t 04215g0sc805…      # the key itself
```

An attested caller has no parent to resolve through, so it needs a key. Putting the key in
a CI variable works and ages badly: **a key rotates, a name does not.** So a host can
publish the answer, and the controller prints exactly what to publish:

```
controller `cm-c` listening as 04215g0sc805rp6qj6cicckdtsaku7fbeaaphe65oedrfeb0a5bg
reachable at: 10.20.2.196:59000, 127.0.0.1:59000
publish at /.well-known/cm-controller: {"key":"04215g0sc…","hints":["10.20.2.196:59000"]}
attestations accepted from: github (acme/*)
```

Drop that one line at `https://cm.acme.com/.well-known/cm-controller` — a static file on
anything that already serves HTTPS — and `CM_CONTROLLER=cm.acme.com` keeps working through
a key rotation.

`CM_CONTROLLER_HINTS` still overrides the published addresses, for when discovery is wrong
and you need it to work now.

### Why not DNS

The design note said DNS, and this is a deliberate departure. **A TXT record is
unauthenticated.** Anything that tells a caller *which key to trust* has to come from a
publisher the caller can check, and over HTTPS the CA system already vouches that this
domain said it. With plain DNS, whoever answers the query chooses which controller you
dial — and a caller that dialled the wrong one would hand over its attestation and take
orders from a stranger.

The token itself survives that: its audience is the caller's own key, so it cannot be
replayed elsewhere. But a fake controller handing out fake grants is bad enough.

DNS with DNSSEC would be equivalent, and the document is the same either way, so adding it
later is small. HTTP is accepted for private networks and warns that nothing vouches for
the answer.

A bare word that is none of the three forms is refused with all three named, rather than
failing later on a decode error.

## A laptop, rather than a runner

A laptop is not ephemeral, and making it fetch a token before every test run would mean a
browser round trip nobody would tolerate for long. So it proves itself **once** and leaves a
key behind:

```sh
$ cm auth login --at cm.acme.com --note "dana's laptop"
cm.acme.com publishes 3p52slsq290ah11lke9het48m85t3tus7rd38ah93p9q3s2g5r60
minted 3h8rvpc7v29i3k87vorkg4htnf7d5ijummbg3i50c4crdvp3ivrg for cm.acme.com
enrolled. `cm t cm.acme.com` needs no token from now on

$ cm t cm.acme.com --plea nightly --count 2 --need linux
enrolled with cm.acme.com as 3h8rvpc7v29i3k87vorkg4htnf7d5ijummbg3i50c4crdvp3ivrg
granted 2 machine(s) as r1
```

**After enrolling there is no session.** No token, no expiry, no refresh, nothing to
rotate and nothing to leak — a later request is authenticated by the connection it arrives
on, because dialling from a key is possession of it.

```sh
$ cm auth status
cm.acme.com
  key 3h8rvpc7v29i3k87vorkg4htnf7d5ijummbg3i50c4crdvp3ivrg
  you are okta:dana@acme.com
    3h8rvpc7v29i3k87… via okta (this one) — dana's laptop
```

"As whom" is *asked*, not remembered. Only the service knows whether a key is still
trusted, and printing a stale answer would be worse than printing none. A service that is
merely unreachable says so, rather than reporting you as not enrolled.

### A fresh key per service

One keypair per service, and nowhere else — the same rule the substrate follows for peers,
where sirji mints a key per relationship so no two peers can correlate you. One key
everywhere would let two unrelated fleets discover they are talking to the same laptop.

The secret lives in the keystore with every other secret this machine holds; a second place
to keep private keys is a second place to get it wrong.

### Revoking is forgetting

```sh
$ cm auth logout --at cm.acme.com
cm.acme.com has forgotten this key
forgotten locally too
```

The service is asked **first**, then the local copy is deleted. That order matters: a key
deleted here but still remembered there is a credential nobody can revoke, because the only
thing that could ask has thrown away the means to. If the service cannot be reached the key
is left in place and you are told which key an operator would need to remove.

From the other side, an operator revokes by removing a line from `enrolled.toml`. That is
the only kind of revocation that is instant and cannot be replayed around — there is no
token still valid for another hour.

### A build token cannot enrol

```
$ CM_ATTEST_CMD='…github token…' cm auth login --at cm.acme.com
refused: tokens from this issuer prove who is asking but may not enrol a key —
         it proves a job rather than a machine
```

An issuer must say `enrol = true`, and the default is off. A CI token proves a
**repository**, and a repository is not a machine: letting build tokens enrol would put one
permanent key per project in `enrolled.toml`, which is the thing attestation exists to
avoid.

So a host typically names two issuers — the CI platform, for jobs, and an identity provider,
for people:

```toml
[[issuer]]
name = "okta"
url = "https://acme.okta.com"
jwks = "https://acme.okta.com/oauth2/v1/keys"
subject = "email"
allow = ["*@acme.com"]
enrol = true
```

{: .note }
**Not built:** the RFC 8628 device flow that would make `cm auth login` open a browser by
itself. Today the token comes from the same `CM_ATTEST_CMD` hook, so anything that can print
one for `{audience}` works. The enrolment it performs, and everything above, is built.

## What an attested caller cannot do

**Be an admin.** Admin membership is by key, from a list a host wrote by hand. An attested
caller's key was minted for one run, and an enrolled caller's arrived by somebody proving an
identity rather than by an operator typing it. No claim in a token changes either, and there
are tests for both.

**Be a sibling.** Attestation makes you a known caller, not one of the organisation's own
devices.

**Raise its own limits.** It is weighed against its tenant's policy like anybody else, and
the ceiling, the budget and availability all still apply.

## Trying it without a CI system

`scripts/attest.sh` runs the whole thing locally: an RSA key, a JWKS endpoint served over
HTTP, real signatures, and a caller with no `CM_HOME` at all. It also demonstrates each
refusal — a token minted for somebody else's key, a repository outside `allow`, a tampered
signature, an unknown issuer.
