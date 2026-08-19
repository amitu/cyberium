#!/usr/bin/env bash
#
# `cm policy-test`, end to end, against a stand-in model.
#
# The stand-in reads no prose — it grants what was asked inside whatever numeric limits
# the prompt states. So this cannot check whether a *policy* is any good; it checks the
# machinery around one: cases load, run, compare, and set an exit code, defaults apply,
# and a failure is reported with the rationale attached.
#
# The one property here worth more than the rest: `policy-tests/` must not reach the
# model. A folder is sent verbatim, so a case inside it would hand over the answer key
# with the question, and every test would pass while checking nothing. That failure would
# be silent and total, so it is asserted against the prompt the model actually received.
#
# For whether a policy says what its author meant, see examples/policy — those cases are
# about prose and need a real model.
#
#   SIRJI=/path/to/sirji CM=/path/to/cm scripts/policytest.sh
#
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
LAB=${LAB:-/tmp/cmpolicytest}
CM=${CM:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/cm}
PORT=8801

rm -rf "$LAB"; mkdir -p "$LAB/policy-tests" "$LAB/nivedanas"
say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }
pt() { CM_MODEL_KEY=stand-in CM_MODEL_URL=http://127.0.0.1:$PORT "$CM" policy-test "$LAB" "$@"; }

cat >"$LAB/policy.md" <<'EOF'
# policy.md

```yaml
requesters:
  - everyone
standing_limit: 2
max_limit: 5
reservation_seconds: 60
```

## Who may ask for what

Everybody may name a plea from `nivedanas/`. Dana may not.
EOF

cat >"$LAB/nivedanas/routine.md" <<'EOF'
## Nightly regression

Routine and never urgent.
EOF

python3 "$HERE/fakemodel.py" $PORT allow ask "$LAB/prompt.log" >/dev/null 2>&1 &
echo $! >"$LAB/model.pid"
trap 'kill "$(cat "$LAB/model.pid")" 2>/dev/null || true' EXIT
sleep 1

# Expectations chosen to match what a stand-in that reads only numbers will do, so a
# failure here is the harness being wrong rather than the stand-in being simple.
cat >"$LAB/policy-tests/cases.json" <<'EOF'
[
  { "name": "the ask is the ceiling on any grant",
    "asked": 3, "said": {"plea": "nightly-regression"},
    "expect": {"verdict": "allow", "count": 3} },
  { "name": "max_limit bounds a bigger ask",
    "asked": 99, "said": {"plea": "nightly-regression"},
    "expect": {"verdict": "counter", "count": 5} },
  { "name": "nothing free means nothing granted",
    "asked": 3, "fleet": {"capable": 4, "free": 0, "rates": []},
    "expect": {"verdict": "deny"} },
  { "name": "a spent budget buys nothing",
    "asked": 3, "money": {"budget": 10, "spent": 10},
    "expect": {"verdict": "deny"} },
  { "name": "a case says only what it is about",
    "expect": {"count": 1} }
]
EOF

say "the cases run, and the machinery agrees with itself"
pt | sed 's/^/  /'

say "a case that is wrong fails, says what it expected, and quotes the rationale"
cat >"$LAB/policy-tests/wrong.json" <<'EOF'
{ "name": "deliberately wrong: claims 4 where the ask is 3",
  "asked": 3, "expect": {"count": 4} }
EOF
set +e
pt --only deliberately | sed 's/^/  /'
echo "  exit status: $?  (non-zero, or CI would never notice)"
set -e
rm "$LAB/policy-tests/wrong.json"

say "--only picks one case, --repeat asks the same question more than once"
pt --only "ask is the ceiling" --repeat 3 | sed 's/^/  /'

say "the answer key never reached the model"
python3 - "$LAB/prompt.log" <<'PYX'
import json, sys
def check(label, ok):
    print(f"    {'ok  ' if ok else 'FAIL'} {label}")

bodies = [json.loads(line) for line in open(sys.argv[1]) if line.strip()]
prompts = [b["system"][0]["text"] + b["messages"][0]["content"] for b in bodies]
print(f"    ({len(prompts)} prompt(s) sent)")
whole = "\n".join(prompts)
# The words a case is made of. None of them belong in a prompt.
for leak in ["expect", "at_most", "at_least", "policy-tests", "cases.json", "deliberately"]:
    check(f"no trace of {leak!r}", leak not in whole)
# What should be there: the policy, and the plea the case declared.
check("the policy did arrive",           "Dana may not" in whole)
check("so did the pleas",                "Routine and never urgent" in whole)
check("and what the case declared",      "plea: nightly-regression" in whole)
PYX

say "a case that expects nothing is refused, not quietly passed"
cat >"$LAB/policy-tests/empty.json" <<'EOF'
{ "name": "expects nothing at all", "asked": 1, "expect": {} }
EOF
set +e
pt 2>&1 | tail -1 | sed 's/^/  /'
set -e
rm "$LAB/policy-tests/empty.json"

say "done — lab in $LAB"
