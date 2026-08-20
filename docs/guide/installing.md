---
title: Install
parent: Guide
nav_order: 1
---

# Install

You need two commands: `sirji`, which handles identity, and `cm`, which handles machines.

## From a release

Binaries are published for macOS and Linux, on both architectures:

```sh
# pick the one that matches: aarch64-apple-darwin, x86_64-apple-darwin,
# x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu
V=0.1.0
T=aarch64-apple-darwin
curl -sSL "https://github.com/amitu/cyberium/releases/download/v$V/cm-$T.tar.gz" \
  | tar xz && sudo mv cm /usr/local/bin/
```

No Windows build yet. The daemon uses a Unix socket and the worker shells out to `sh`, so
it is a port rather than a build flag — see the design notes if you need it.

## From source

```sh
cargo install --git https://github.com/amitu/cyberium cyberium
cargo install --git https://github.com/amitu/sirji sirji
```

The toolchain is pinned in `rust-toolchain.toml`, so a checkout builds with the same
compiler CI uses. Building against a newer one usually works and is not what gets tested.

## What a machine needs

**A controller** needs a model. There is no unweighed mode:

```sh
export CM_MODEL_KEY=…            # required — a controller will not start without one
export CM_MODEL=claude-sonnet-5  # optional
export CM_MODEL_URL=…            # optional: point at your own endpoint
```

That is deliberate, and the reasoning is in [Writing a policy](policy.html): a controller
that came up without a model would be one that cannot read anybody's policy, and finding
that out from a developer's CI log is the wrong place.

**A worker** needs whatever the tests need — a runtime, browsers, `git` — plus `git` itself
if callers use `--repo`. Workers fetch code themselves rather than receiving it, so a
worker with no `git` can still run commands but cannot check anything out.

**A caller** needs nothing but `cm` and an identity.

## Check it

```sh
$ cm whoami
cm-c 5lljf7j7vvvj8pmnd9j1uh82lb984j3bmifs12n0qqeens6mfkpg

$ sirji doctor
```

`sirji doctor` is worth running once on any network you have not used before. It checks DNS,
UDP egress and each relay in turn, and names the link that is broken rather than reporting
that the network is unavailable. It was written because a corporate proxy re-signing TLS
looks exactly like a peer being offline, and the difference matters.
