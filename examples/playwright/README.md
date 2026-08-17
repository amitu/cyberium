# A Playwright suite that knows nothing about cm

Thirteen tests and a config. Look through `tests/` and `playwright.config.ts` and you
will not find a fixture, a reporter, or an import that would tell you this suite gets
distributed — because it does not. That is the point.

The only trace of cm anywhere is one line of `package.json`:

```jsonc
{ "scripts": { "test": "cm-playwright" } }
```

## Run it

```sh
npm install
npm test
```

```
  13 passed (5.3s)
```

Thirteen tests, one after another, on this machine. No `CM_CONTROLLER` is set, so
`npm test` is exactly `playwright test` — which is what makes the plugin safe to
commit before anyone has a fleet.

## Run it on a fleet

Bring some machines up (`scripts/fleet.sh` in the repo root does this locally), then
run **the same command**:

```sh
CM_CONTROLLER=cm-c@acme CM_SHARDS=3 CM_NEED=node20 npm test
```

```
machines will fetch 1c2b537b5ade from git@github.com:amitu/cyberium.git
granted 3 machine(s) as r2
[cm-w-1] fetching 1c2b537b5ade… from git@github.com:amitu/cyberium.git
[cm-w-2] fetching 1c2b537b5ade… from git@github.com:amitu/cyberium.git
[cm-w-3] fetching 1c2b537b5ade… from git@github.com:amitu/cyberium.git
[cm-w-1] $ npm ci
  cm-w-1 finished shard 1 with success
  cm-w-2 finished shard 2 with success
  cm-w-3 finished shard 3 with success
merging 3 shard report(s)
  13 passed (2.6s)
```

Each machine fetched this repo itself — none of them had it — into a checkout of its
own, which was deleted when the reservation ended. Since it fetches a **commit**,
the thing being tested is whatever you last pushed, not what is on your disk; the
plugin warns you when those differ.

## Watch it go red

A distributed runner that loses a failure is worse than no runner at all — CI goes
green on a suite that did not pass. So the example ships a test that fails on demand:

```sh
CM_CONTROLLER=cm-c@acme CM_SHARDS=3 CM_NEED=node20 CM_ENV=CM_EXAMPLE_FAIL=1 npm test
```

```
  cm-w-3 finished shard 3 with exit 1
  ✘  13 tests/arithmetic.spec.ts:21:5 › the one that fails when asked to
  1 failed
  12 passed
```

and `npm test` exits 1.

Note that the failure had to be *sent* in `CM_ENV`: shards do not inherit your
shell's environment, because a worker is another machine.
