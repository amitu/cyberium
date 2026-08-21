---
title: Shipping a policy
parent: Guide
nav_order: 10
---

# Shipping a policy

The policy lives in a repository. The controller does not share a filesystem with it.

```sh
cm upload-policy cm-c@acme .
```

```
sending 3 file(s) from .
  nivedanas/noisy-users/experiments.md
  nivedanas/routine.md
  policy.md
replaced — the next plea is weighed against this
```

The natural shape is a repository per organisation — `<org>/cyberium` — holding the policy,
the pleas and the [test cases](testing-policy.html), with CI that tests them and uploads on
merge. Every other repository then needs nothing but `cm t`.

## Who may

Only an admin named in [`tenant.toml`](tenants.html#members-and-admins):

```
$ cm upload-policy cm-c@acme .
refused: dana may run tests for `payments` but not change its rules — whoever runs
         this controller decides that, not the rules themselves
```

That last clause is the whole point. If a policy named its own admins, anybody who could edit
it could add themselves.

## What it does

**Replaces, never merges.** The folder *is* the policy, so a merge leaves files behind that
nobody remembers writing and no repository contains — the controller would enforce a mixture
that exists nowhere. Delete a plea locally and it is gone from the controller on the next
upload.

**Validated before it replaces anything.** Every path is checked, then the folder is staged
and parsed the way a plea will parse it, and only swapped in if it reads:

```
$ cm upload-policy cm-c@acme .
refused: the uploaded policy could not be read: reading the grants block in …
```

and the policy that works is still in force. A policy accepted and *then* found unreadable
takes the tenant down at its next request, a long way from where the mistake was made.

**Paths are not trusted.** They came from another machine, so only their shape is. Absolute
paths, `..` in any component, and dotfiles are all refused; subdirectories are fine, because
`nivedanas/support/escalation.md` is a real thing somebody wants.

**Some files are never accepted.** `tenant.toml` — a tenant that could overwrite it could
raise its own ceiling. `spend.log` — one that could overwrite it could forget what it had
spent. And `policy-tests/` stays home: the controller does not run them, and they hold the
expected answers.

## Nothing restarts

The controller re-reads the folder immediately, and the next plea is weighed against what you
just sent. If the re-read fails the upload is reported as failed, because a controller still
weighing pleas against the folder it replaced would be applying a policy that no longer
exists.
