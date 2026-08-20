---
title: Testing a policy
parent: Design notes
nav_order: 2
---

# Testing prose

> **Status: built.** `cm policy-test`, the case format, the four expectation shapes,
> `--repeat` and `--only` all run. What is **not** built: a server-side gate that refuses an
> upload whose cases do not pass, and a spread report across repeated runs.

A policy whose prose nobody can test is prose nobody will dare edit. That is the whole
argument for this, and it is worth being precise about what "test" can and cannot mean when
a model is doing the deciding.

## What is actually determinate

It is tempting to write off a model-decided policy as untestable and reach for hedges. That
gets the shape wrong. Three separate things are going on, and only one of them is soft:

**The output is a contract.** The model is asked for one JSON object with three fields, and
anything else is an error rather than a guess. See [the answer's
schema](policy.html#the-answer-is-a-contract-not-a-conversation). Nothing downstream has to
parse prose or infer intent.

**The bounds are arithmetic.** Whatever comes back is clamped to the ceiling, the ask,
availability and the budget. Those are not opinions, and a test of them is an ordinary unit
test — there are several.

**The judgement is the soft part**, and it is the only soft part: *given these rules, does
this plea deserve four machines or two?* That is what `cm policy-test` measures, and the
right way to measure it is to run it.

Temperature is zero, so the same plea against the same fleet gets the same answer and a case
can be written at all. That is a property worth having and not a guarantee about anything
else — which is why `--repeat` exists rather than a paragraph explaining that models vary.

## The case format

```json
{
  "name": "urgency without an incident identifier is not an incident",
  "caller": "dana",
  "asked": 6,
  "said": { "plea": "production-incident", "why": "this is extremely urgent!!" },
  "facts": { "plan": "contractor" },
  "fleet": { "capable": 8, "free": 6, "rates": [1, 1, 2, 2, 8, 8] },
  "money": { "budget": 6000, "spent": 5200, "window": 86400 },
  "expect": { "at_most": 4 }
}
```

Everything but `name` and `expect` defaults, so a case about a *rule* does not have to
describe hardware.

**`fleet` and `facts` are in the fixture, and that is the payoff rather than the
complication.** Contention becomes testable without a contended fleet: *"does an incident
still win when four machines are free"* is the question you most want answered and can never
stage live. Same for a plan or a group — a rule that turns on `plan: contractor` cannot be
tested without saying who is asking.

## Expectations as vague as the rule

| Shape | Checks |
|---|---|
| `count` | exactly this many |
| `at_most` / `at_least` | a bound |
| `verdict` | `allow`, `counter` or `deny` |

*"Counted back towards the standing limit"* is a real sentence to write, and `at_most` checks
it without inventing a number the policy never named. A test stricter than the rule it checks
is a test that fails for being right.

`verdict` is what the **caller** experiences, not the model's word for it: fewer than asked is
a counter however it happened, and nothing at all is a denial whether the model refused or a
clamp took it. The two are [not distinguished in
effect](policy.html#the-answer-is-a-contract-not-a-conversation) anyway.

Two kinds of case are refused at load rather than run. One that expects **nothing** would
pass against any answer, which is worse than no test because it looks like coverage. One that
**cannot pass** — `at_least: 8` against an ask of 6, since the ask is a hard ceiling — fails
for being impossible rather than for anything being wrong, and the failure would not say so.
The example folder in this repository contained the second kind until the check was added,
which is the usual argument for adding one.

## `--repeat`, and what it is really for

"Does this rule hold" and "does this rule hold **every time**" are different questions, and
only the second tells you whether prose is written clearly enough to depend on. A rule that
passes four times in five is a rule to rewrite, not a flake to retry — the wording is
ambiguous and a human reader would have been unsure too.

Reporting the spread rather than the first failure would be better, and is not built.

## The two things worth getting right

**The cases must never reach the model.** A folder is sent verbatim, so a case inside it would
hand the answer key over with the question — and every test would pass while checking nothing.
That failure would be silent and total, so `policy-tests/` is excluded from what is sent and
`scripts/policytest.sh` asserts it against the prompt the model actually received.

**The decision must be the controller's own.** One `weigh` function, shared. A second
implementation would eventually disagree with the first and be believed: a policy test that
passes against a slightly different decision than the fleet makes is worse than no test. Same
rule as the dry run sharing `choose`.

## What a policy test does not cover

`cm policy-test` runs against a *folder*, with no deployment behind it. So
[`Directory::reviewed`](policy.html#the-post-processor) — a hosted deployment's own look at
the answer — is not called. That is the honest boundary: a policy test checks the policy, and
whatever a deployment does afterwards is that deployment's to test.

Worth knowing if you put a rule in a post-processor that a tenant could otherwise have read
about in their own files. If they cannot test it, they will be surprised by it.

## The real value

Catching the day the model changes under you. Your wording did not move; the decisions did.
Nothing else in the system would tell you, and a policy is a thing you are meant to be able
to leave alone for six months.

---

For how to run it, see the [guide](../guide/testing-policy.html).
