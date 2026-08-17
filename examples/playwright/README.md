# A Playwright suite that knows nothing about cm

Thirteen tests, one config, no cm-specific code anywhere. That is the whole point of
the example: look through `tests/` and `playwright.config.ts` and you will not find a
fixture, a reporter, or an import that would tell you this suite gets distributed.

## Run it the ordinary way

```sh
npm install
npx playwright test
```

Thirteen tests, one after another, about five seconds.

## Run it on a fleet

With a controller and three machines up (`scripts/fleet.sh` in the repo root brings
some up locally):

```sh
CM_HOME=… cm playwright
```

The fleet settings come from the `cm` key already in `package.json`:

```jsonc
{ "cm": { "controller": "cm-c@acme", "need": ["node20"], "shards": 3 } }
```

Each machine runs `npx playwright test --shard=i/3 --reporter=blob`, the blobs come
back over sirji, and Playwright's own `merge-reports` stitches them into one report:

```
  ✓   1 tests/arithmetic.spec.ts:12:7 › case 1 adds up (406ms)
  …
  13 passed (2.6s)
```

## Watch it go red

A distributed runner that loses a failure is worse than no runner at all — CI goes
green on a suite that did not pass. So the example ships a test that fails on demand:

```sh
cm playwright --env CM_EXAMPLE_FAIL=1
```

```
  cm-w-1 finished shard 1 with success
  cm-w-2 finished shard 2 with success
  cm-w-3 finished shard 3 with exit 1
  ✘  13 tests/arithmetic.spec.ts:21:5 › the one that fails when asked to
  1 failed
  12 passed
```

and `cm` exits 1.

Note the `--env`: shards do **not** inherit your shell's environment, because a
worker is another machine. Anything the run needs has to travel with the job.
