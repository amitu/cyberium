# @cyberium/playwright

Run an existing Playwright suite across a [cm](https://github.com/amitu/cyberium)
fleet. **The suite is not modified** — no fixture, no reporter, no globalSetup, and
not one line of any spec file.

```sh
npm i -D @cyberium/playwright
npx cm-playwright --shards 4 -- --project qa
```

## Why there is nothing to add to your tests

Playwright already knows how to split a run: `--shard=i/N` on N machines, each
writing a blob report, then `merge-reports` stitching them into one. That has always
been the shape; what has been missing is somebody to find the machines.

So that is all cm does. It asks a controller for N machines matching the
capabilities you need, starts an ordinary Playwright run on each — the same command
you would type — brings the blob reports back, and hands them to Playwright's own
`merge-reports`. Every shard is a normal Playwright process that has no idea it is
part of anything.

This is also why there is no reporter or fixture in this package. Distribution
happens *outside* the Playwright process. A plugin that installed a hook would be
claiming an influence over the run it does not have.

## Configuring it

Either in `package.json`:

```jsonc
{
  "cm": {
    "controller": "cm-c@acme",
    "need": ["linux", "node20"],
    "shards": 4
  }
}
```

or next to everything else about the run, in the Playwright config:

```ts
import { defineConfig } from "@playwright/test";
import { cm } from "@cyberium/playwright";

export default defineConfig({
  ...cm({ controller: "cm-c@acme", need: ["linux", "node20"], shards: 4 }),
  // the rest of your config, unchanged
});
```

`cm()` returns an empty fragment and writes `cm.generated.json` beside the config —
`cm playwright` runs outside the Playwright process, so it cannot see values you
return, and evaluating a TypeScript config from a Rust binary would mean shipping a
TypeScript interpreter to read four fields.

Flags beat config; config beats nothing.

## Environment

**Shards do not inherit your shell's environment.** A worker is another machine, and
a tool that pretended otherwise would produce suites that pass locally and fail on
the fleet for reasons nobody can see. Pass what the run needs:

```sh
npx cm-playwright --env GRPC_SERVER=off --env APP_ENV=staging -- --project qa
```

Secrets deserve a moment's thought before they go in there: `--env` sends a value to
a machine somebody else administers.

## Exit codes

The run's verdict passes straight through. A failing shard is a failing run — a
wrapper that turns a red suite green is the single worst thing a test tool can do.

## Finding the binary

`cm-playwright` shells out to `cm`. It looks in `$CM_BIN`, `node_modules/.bin`,
`~/.cargo/bin`, the usual prefixes, then `PATH`. If none of those has it, it says so
rather than guessing at something that merely looks plausible.

## License

Apache-2.0.
