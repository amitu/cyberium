# @cyberium/playwright

Run your existing Playwright suite across a [cm](https://github.com/amitu/cyberium)
fleet, from `npm test`.

```sh
npm i -D @cyberium/playwright
```

```jsonc
// package.json
{ "scripts": { "test": "cm-playwright" } }
```

That is the whole installation. **No spec file changes, no fixture, no reporter, no
import.** People keep running `npm test`.

## Configured by environment

`npm test` has nowhere to put flags, and a CI job has nowhere else to put
configuration, so everything is an environment variable:

| Variable | Meaning |
|---|---|
| `CM_CONTROLLER` | `name@org` of the controller. **Unset ⇒ runs locally, as normal.** |
| `CM_SHARDS` | how many machines to ask for (default 4) |
| `CM_NEED` | capabilities, comma separated: `linux,node20` |
| `CM_ENV` | environment for the shards: `GRPC_SERVER=off,APP_ENV=staging` |
| `CM_WHY` | the reason the controller weighs against its policy |
| `CM_RUNNER` | how to start Playwright (default `npx playwright test`) |
| `CM_HOME` | which cm identity to use — see `cm init` |
| `CM_BIN` | path to the cm binary, if it is somewhere unusual |
| `CM_REPO` | where machines fetch the code (default: this checkout's `origin`) |
| `CM_REF` | which commit (default: `HEAD`) |
| `CM_DIR` | subdirectory to run in (default: where you are, within the repo) |
| `CM_SETUP` | install step on each machine (default `npm ci`) |
| `CM_NO_CLONE` | use a workspace already on the machine instead |

Playwright's own arguments pass through untouched:

```sh
npm test -- --project qa --grep @smoke
```

**With `CM_CONTROLLER` unset, `npm test` is exactly `playwright test`.** That is what
makes this safe to commit: the laptop with no cm, the CI job that has not been given
a fleet yet, and the contributor who has never heard of any of this all keep working.

## The machines fetch the code themselves

A worker starts with nothing. It does not have your repo, so each shard clones the
commit you are on into a checkout of its own, runs `npm ci`, and has the whole thing
deleted when the reservation ends. Nothing is reused between runs — a working tree
left over from someone else's job is how a green suite starts depending on what ran
before it.

All of it is derived from the checkout you are standing in: `origin` for the repo,
`HEAD` for the commit, and your position within the repo for the subdirectory. The
usual case needs no configuration.

Two warnings you may see, both worth reading:

- *cannot confirm … is pushed* — the machines fetch from a remote, so a commit only
  you have is a commit they cannot get. It is a warning rather than a refusal
  because it is checked against local remote-tracking refs, which a checkout that
  pushes by URL may simply not have.
- *you have uncommitted changes* — the fleet tests the **commit**, not your disk.

The machines need read access to the repo: a deploy key, an SSH agent, or a public
URL. Note that `CM_REPO` travels to them verbatim, so a URL with a token in it hands
that token to whoever runs the machine.

Setup is now the slowest part of a run — `npm ci` on three machines is most of the
wall clock for a short suite. That is where caching should go next; nothing about it
is load-bearing here.

## What it actually does

Playwright has always known how to split a run — `--shard=i/N`, a blob report each,
`merge-reports` at the end. What was missing was somebody to find the machines.

So this asks `cm test` for N machines with the capabilities you named, has each run
the ordinary command you would have typed with its shard number filled in, brings
the blob reports back, and hands them to Playwright's own `merge-reports`. Every
shard is a normal Playwright process that has no idea it is part of anything.

`cm` itself knows nothing about Playwright — it gets machines and runs a command.
Everything runner-shaped is in this package, because a `cm` that understood
Playwright would owe the same favour to jest, pytest and whatever comes next.

`{shards}` is the number of machines actually **granted**, not the number asked for,
so a controller countering with three still produces a correct three-way split.

The merge runs with `-c <your playwright config>`. Blob reports record the absolute
directory the tests ran in and `merge-reports` refuses to combine ones that disagree
— which machines with their own checkouts always do. Anchoring to your config also
gives the report paths in the checkout you are about to go and look at.

## Environment, again, because it bites

Shards do **not** inherit your shell's environment. A worker is another machine.
Anything the run needs must travel with the job, in `CM_ENV`.

The first real suite this ran caught it: two shards started the same fixture server
because the variable that would have disabled it never arrived, and the second hit
`EADDRINUSE`. Secrets deserve a thought before they go in there — `CM_ENV` sends a
value to a machine somebody else administers.

## Exit codes

The suite's verdict passes straight through: a failing shard is a failing run, and
the merge cannot turn it green. A distributed runner that loses a failure is worse
than no runner — CI goes green on a suite that did not pass.

## License

Apache-2.0.
