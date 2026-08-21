---
title: Reference
parent: Guide
nav_order: 14
---

# Reference

`cm` with no arguments prints all of this too, and the binary is the authority if these ever
disagree.

## Commands

| Command | What it does |
|---|---|
| `cm init --parent <invite> [--root <dir>]` | create `$CM_HOME` and enrol with the sirji that issued the invite |
| `cm whoami` | this device's name and key — what `cm admin add` wants |
| `cm controller` | own the fleet: availability, allocation, timeouts |
| `cm worker` | offer this machine |
| `cm test <name@org> --ping` | check identity, resolution, dial and ticket; take nothing |
| `cm test <name@org> ["why"]` | ask for machines, use them, give them back (`cm t`) |
| `cm test <controller-id52>` | the same, proving yourself by [attestation](ci.html) instead |
| `cm test <host>` | the same, asking the host which controller it runs |
| `cm tenant add <name>` / `cm tenant list` | onboard whoever this controller serves |
| `cm admin add <name> <id52>` / `cm admin list` | who may look inside |
| `cm admin fleet` / `reservations` / `spend` | look inside a running controller |
| `cm auth login --at <host>` | prove who you are once, and leave a key behind |
| `cm auth status` | what this machine is enrolled with, and as whom |
| `cm auth logout --at <host>` | the service forgets the key; then so does this machine |
| `cm policy-test [<dir>]` | run `<dir>/policy-tests/` against `<dir>`'s rules |
| `cm upload-policy <name@org> [<dir>]` | replace what a tenant has written down |

## `cm worker`

| Flag | What it does |
|---|---|
| `--controller <name>` | which sibling to register with |
| `--can <cap>` | a capability. Repeatable |
| `--rate N` | credits per minute while held. Default 1, never free |
| `--pre <cmd>` | make the machine fit before each tenancy |
| `--post <cmd>` | take back what the last tenant left. Failure removes the machine |

## `cm test`

Cyberium's own, because it acts on them mechanically:

| Flag | What it does |
|---|---|
| `--count N` | how many machines. A ceiling on the grant |
| `--need <cap>` | required capability. Repeatable; all must match |

The run:

| Flag | What it does |
|---|---|
| `--run <cmd>` | what to run on each machine |
| `--repo <url>` | fetch this first |
| `--ref <commit>` | which commit |
| `--dir <subdir>` | run below the repo root |
| `--setup <cmd>` | run once before the command |
| `--cwd <dir>` | run here instead, when the machine already has the code |
| `--env K=V` | extra environment. Repeatable |
| `--collect <path>` | bring this back. Repeatable |
| `--artifacts <dir>` | where returned files land. Default `cm-artifacts` |
| `--dry-run` | what would I get? Takes nothing |
| `--abandon` | keep the grant and walk away, to watch it time out |

Substituted in `--run`, `--env` and `--collect`: `{shard}` 1-based, `{index}` 0-based,
`{shards}` the total **granted**.

Anything else — `--plea x`, `--incident INC-1`, `--urgent` — is a
[declaration](running-tests.html#saying-things-to-your-own-policy) your own policy reads.
cyberium attaches no meaning to any of it.

## `cm tenant add`

| Flag | What it does |
|---|---|
| `--ceiling N` | your cap on them, whatever their policy says |
| `--credits N` / `--window SECS` | budget per rolling window |
| `--member <alias>` | a caller alias. Repeatable |
| `--admin <alias>` | may change the rules. Also a member. Absent means nobody |
| `--note <text>` | for whoever has to work out later why this exists |

## `cm policy-test`

| Flag | What it does |
|---|---|
| `--repeat N` | run each case N times; all must pass |
| `--only <substring>` | just the cases whose name contains this |

## Environment

**Everywhere**

| Variable | What it does |
|---|---|
| `CM_HOME` | this device's home. Default `~/.cm` |
| `SIRJI_HOME` | the sirji identity to use |

**Controller**

| Variable | What it does |
|---|---|
| `CM_MODEL_KEY` | **required.** No key, no controller |
| `CM_MODEL` | default `claude-sonnet-5` |
| `CM_MODEL_URL` | point at your own endpoint |

**Caller**

| Variable | What it does |
|---|---|
| `CM_SAY` | declarations from CI: `plea=nightly,incident=INC-1` |
| `CM_ATTEST` | which built-in integration mints the token. Default `github` |
| `CM_ATTEST_CMD` | any other provider: a command printing a token for `{audience}` |
| `CM_CONTROLLER_HINTS` | addresses to dial, overriding what a host published |

**The Playwright plugin** — see [Playwright](playwright.html) for the full list, including
`CM_CONTROLLER` (required), `CM_SHARDS`, `CM_NEED`, `CM_ENV`, `CM_WHY`, `CM_REPO`, `CM_REF`,
`CM_DIR`, `CM_SETUP`, `CM_NO_CLONE`, `CM_DRY_RUN`, `CM_RUNNER`, `CM_BIN`.

## Files

| File | What is in it |
|---|---|
| `$CM_HOME/config.toml` | this device: name, key, parent, root |
| `<root>/admins.toml` | who may look inside. Host-written, by key |
| `<root>/issuers.toml` | **host-owned**: whose attestations this controller believes |
| `<root>/enrolled.toml` | written by the controller: keys it agreed to remember |
| `$CM_HOME/services.toml` | this machine's side: one key per service enrolled with |
| `<root>/tenants/<name>/tenant.toml` | **host-owned**: ceiling, members, admins, credits, `[facts]` |
| `<root>/tenants/<name>/policy.md` | **tenant-owned**: the fenced block, and the rules |
| `<root>/tenants/<name>/…` | anything else the tenant writes. All of it is the policy |
| `<root>/tenants/<name>/spend.log` | append-only ledger, unix seconds |
| `policy-tests/*.json` | cases. Never sent to the model |
| `/.well-known/cm-controller` | served by a host: `{"key": "…", "hints": […]}` |

## Scenarios

Every one of these runs live, against a stand-in model, and prints what happened:

| Script | What it exercises |
|---|---|
| `scripts/fleet.sh` | a whole fleet: capabilities, checkouts, artifacts, reclaim, hygiene |
| `scripts/model.sh` | the model call: bounds, refusals, faults, prompt contents, uploads |
| `scripts/policytest.sh` | `cm policy-test`, including that cases never reach the model |
| `scripts/hosted.sh` | a custom controller with its own directory |
| `scripts/attest.sh` | a caller with nothing enrolled, and every way a token is refused |
