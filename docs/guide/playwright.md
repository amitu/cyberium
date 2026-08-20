---
title: Playwright
parent: Guide
nav_order: 4
---

# Playwright

Your suite runs across a fleet without changing a line of it. No cyberium import, no config
change, no `cm playwright` subcommand to learn — `npm test` distributes itself.

```json
{
  "scripts": {
    "test": "cm-playwright"
  }
}
```

```sh
npm i -D @cyberium/playwright
CM_CONTROLLER=cm-c@acme npm test
```

That is the whole integration. Verified against a real 1,953-test suite, sharded across a
fleet that fetched the repository itself, and merged back into one report.

## Why it is a plugin and not a subcommand

A `cm playwright` command would owe the same favour to jest, pytest, go test, and whatever
comes next — and every one of those would put knowledge of a test runner inside the thing
that allocates machines. So runners plug in *on top of* `cm test`, and this one is 250 lines
of Node that shells out.

Everything here is available to any other runner by doing the same.

## What it does

1. Works out the repository and commit from your git checkout, so machines fetch the same
   code you have.
2. Asks for `CM_SHARDS` machines.
3. Runs `npx playwright test --shard={shard}/{shards} --reporter=blob` on each.
4. Brings every blob report back.
5. Merges them into one HTML report.

The shard count passed to Playwright is what was **granted**, not what was asked for. If a
policy counters six down to three, you get a correct three-way split rather than three
thirds of a six-way one.

## Configuration

All by environment, because a plugin that needed its own config file would be a second place
for the same information to disagree with itself.

| Variable | What it does |
|---|---|
| `CM_CONTROLLER` | **Required.** `name@org` of the controller. |
| `CM_SHARDS` | How many machines to ask for. Default 4. |
| `CM_NEED` | Capabilities, comma separated: `linux,node20`. |
| `CM_RUNNER` | How to start Playwright. Default `npx playwright test`. |
| `CM_ENV` | Environment for the shards: `APP_ENV=staging,GRPC=off`. |
| `CM_WHY` | A reason in your own words. Default: this package's name. |
| `CM_SAY` | Anything else your policy reads: `plea=nightly,incident=INC-1`. |
| `CM_REPO` | Where machines fetch the code. Default: this checkout's `origin`. |
| `CM_REF` | Which commit. Default: `HEAD`. |
| `CM_DIR` | Subdirectory to run in. Default: where you are, within the repo. |
| `CM_SETUP` | Install step on each machine. Default `npm ci`. |
| `CM_NO_CLONE` | Use a workspace already on the machine instead. |
| `CM_DRY_RUN` | Ask what you would get, run nothing, hold nothing. |
| `CM_HOME` | Which cm identity to use. |
| `CM_BIN` | Path to `cm`, if it is somewhere unusual. |

**`CM_CONTROLLER` unset is an error, not a fallback to running locally.** Quietly running
the suite here would be indistinguishable from having distributed it, and you would find out
which had happened from the wall clock.

## From CI

```yaml
- run: npm test
  env:
    CM_CONTROLLER: cm-c@acme
    CM_SHARDS: "8"
    CM_SAY: "plea=pre-merge-check,pr=${{ github.event.number }}"
```

`CM_SAY` is how a job tells your policy what kind of run this is. cyberium attaches no
meaning to those pairs — [your policy](policy.html) does, and it can say things like *"a
pre-merge check on a draft PR waits"* without anybody shipping code for it.

## Two things that will bite you

**Push before you run.** Machines fetch `CM_REF` from `CM_REPO`. An unpushed commit is a
commit they cannot see, and the failure looks like a checkout problem rather than a missing
push. The plugin warns rather than refusing, because the check it would need — comparing
against a remote-tracking ref — is wrong in a fresh CI checkout.

**Blob reports embed an absolute `rootDir`.** `merge-reports` refuses reports whose paths
came from a different machine, so the plugin passes `-c <your config>` when merging. Worth
knowing if you script the merge yourself.

## A lockfile is required

`npm ci` — the default `CM_SETUP` — needs `package-lock.json` committed. Obvious in
hindsight; it cost an hour the first time.
