#!/usr/bin/env bash
#
# A caller with nothing enrolled, proving who it is anyway.
#
# This is the CI case. There is no `cm init`, no parent, no entry in anybody's roster: the
# caller mints a keypair for the run, gets a token whose **audience is that key**, dials the
# controller by id52, and throws the key away. `scripts/mint.sh` stands in for the platform's
# token endpoint, signing with a real RSA key that a real JWKS endpoint publishes.
#
# What is worth proving here, in order of how badly it would matter if it broke:
#
#   1. A token for somebody else's key is refused. This is the whole design: a token lifted
#      from a build log names an audience the thief cannot dial from.
#   2. A repository outside `allow` is refused, even with a perfectly valid token.
#   3. A tampered token is refused — the signature is actually checked, against keys
#      actually fetched over HTTP.
#   4. The claims arrive as **attested** facts, so a policy can turn on the repository and
#      the event without cm knowing what either means.
#   5. An attested caller is never an admin, whatever it claims.
#
#   SIRJI=/path/to/sirji scripts/attest.sh
#
set -euo pipefail

LAB=${LAB:-/tmp/cmattest}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
CM=${CM:-$ROOT/target/debug/cm}
SIRJI=${SIRJI:-sirji}
HERE=$(cd "$(dirname "$0")" && pwd)
MODEL_PORT=8821
JWKS_PORT=8822

if [ "$CM" = "$ROOT/target/debug/cm" ]; then
  cargo build --quiet --manifest-path "$ROOT/Cargo.toml"
fi

rm -rf "$LAB"; mkdir -p "$LAB"; cd "$LAB"
say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

# --- the issuer: a key, and a JWKS endpoint that publishes it -------------------
openssl genrsa -out issuer.pem 2048 2>/dev/null
# `n` is the modulus, big-endian, base64url. `e` is 65537, which is AQAB, for every key
# openssl generates by default.
MOD=$(openssl rsa -in issuer.pem -noout -modulus | cut -d= -f2)
N=$(printf '%s' "$MOD" | xxd -r -p | openssl base64 -A | tr '+/' '-_' | tr -d '=')
mkdir -p jwks/.well-known
cat >jwks/keys.json <<EOF
{"keys":[{"kty":"RSA","kid":"test-1","alg":"RS256","use":"sig","n":"$N","e":"AQAB"}]}
EOF
(cd jwks && python3 -m http.server $JWKS_PORT >/dev/null 2>&1) &
echo $! >jwks.pid
sleep 1

# --- an organisation, a controller, a worker ------------------------------------
SIRJI_HOME=$LAB/acme $SIRJI init >/dev/null
SIRJI_HOME=$LAB/acme $SIRJI daemon >acme.log 2>&1 &
sleep 4
for d in cm-c w1 w2; do
  INV=$(SIRJI_HOME=$LAB/acme $SIRJI device invite $d | tail -1)
  CM_HOME=$LAB/$d SIRJI_HOME=$LAB/$d $CM init --parent "$INV" --root "$LAB/$d/root" >/dev/null
done

# The tenant lists the *repository* as a member, prefixed by the issuer that vouches for
# it. That prefix is why an attested alias cannot collide with a sirji one.
CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $CM tenant add ci \
  --ceiling 8 --member "github:acme/payments" >/dev/null

cat >"$LAB/cm-c/root/tenants/ci/policy.md" <<'POLICY'
# policy.md

```yaml
requesters:
  - everyone
standing_limit: 2
max_limit: 6
reservation_seconds: 60
```

## What we allow from CI

A pull request build may have up to the maximum; a scheduled run is routine and waits.
POLICY

# Who this controller believes, besides its own sirji. Host-owned, like admins.toml.
cat >"$LAB/cm-c/root/issuers.toml" <<EOF
[[issuer]]
name = "github"
url = "https://issuer.test"
jwks = "http://127.0.0.1:$JWKS_PORT/keys.json"
subject = "repository"
allow = ["acme/*"]
facts = ["ref", "event_name", "workflow"]
EOF

python3 "$HERE/fakemodel.py" $MODEL_PORT allow ask "$LAB/prompt.log" >/dev/null 2>&1 &
echo $! >model.pid
trap 'kill $(cat "$LAB"/*.pid 2>/dev/null) 2>/dev/null || true' EXIT
sleep 1

CM_MODEL_KEY=stand-in CM_MODEL_URL=http://127.0.0.1:$MODEL_PORT \
CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $CM controller >cm-c.log 2>&1 &
sleep 3
grep -E "attestations accepted from" cm-c.log | sed 's/^/  /'

for w in w1 w2; do
  CM_HOME=$LAB/$w SIRJI_HOME=$LAB/$w $CM worker --can linux --rate 1 >$w.log 2>&1 &
done
for _ in $(seq 40); do [ "$(grep -c arrived cm-c.log || true)" -ge 2 ] && break; sleep 1; done

KEY=$(CM_HOME=$LAB/cm-c SIRJI_HOME=$LAB/cm-c $CM whoami | awk '{print $2}')
# From the controller's own line, not by grepping the log for anything that looks like an
# address — the model's URL is in there too, and matching that sent every dial nowhere.
HINTS=$(grep "^reachable at:" cm-c.log | head -1 | sed 's/^reachable at: //')
echo "  controller $KEY at $HINTS"

# What an operator would put on a web server, taken from the line the controller prints.
# The same python server that publishes the issuer's keys stands in for that web server.
mkdir -p jwks/.well-known
grep "^publish at" cm-c.log | head -1 | sed 's/^publish at [^:]*: //' \
  >jwks/.well-known/cm-controller
echo "  publishing: $(cat jwks/.well-known/cm-controller | cut -c1-72)"

# No CM_HOME and no SIRJI_HOME: this caller has nothing on disk at all.
ci() {
  env -u CM_HOME -u SIRJI_HOME \
    CM_CONTROLLER_HINTS="$HINTS" \
    CM_ATTEST_CMD="$1" \
    "$CM" test "$KEY" "${@:2}"
}
mint="bash $HERE/mint.sh $LAB/issuer.pem {audience}"

say "a runner with nothing enrolled asks for machines"
ci "$mint repository=acme/payments ref=refs/pull/41/merge event_name=pull_request" \
   --plea pr-build --count 2 --need linux --dry-run 2>&1 | tail -2 | sed 's/^/  /'

say "the claims arrived as PROVEN, not as something the caller said"
python3 - "$LAB/prompt.log" <<'PYX'
import json, sys
def check(label, ok):
    print(f"    {'ok  ' if ok else 'FAIL'} {label}")
user = json.loads(open(sys.argv[1]).read().strip().split("\n")[-1])["messages"][0]["content"]
proven = user[user.index("ATTESTED"):user.index("THE REQUEST")]
check("the repository is attested",   "repository: acme/payments" in proven)
check("so is the ref",                "ref: refs/pull/41/merge" in proven)
check("so is the event",              "event_name: pull_request" in proven)
check("and which issuer vouched",     "issuer: github" in proven)
check("the caller is the repository",  "caller: github:acme/payments" in proven)
check("none of it is in DECLARED",     "acme/payments" not in user[user.index("DECLARED"):])
PYX

say "a token minted for somebody else's key — the one that matters"
ci "$mint repository=acme/payments" --count 1 --need linux --dry-run 2>&1 \
  | tail -1 | cut -c1-104 | sed 's/^/  ours:  /' || true
# Audience fixed to a key this caller does not hold: exactly a token scraped from a log.
STOLEN=$(bash "$HERE/mint.sh" "$LAB/issuer.pem" \
  5lljf7j7vvvj8pmnd9j1uh82lb984j3bmifs12n0qqeens6mfkpg repository=acme/payments)
set +e
ci "printf %s $STOLEN" --count 1 --need linux --dry-run 2>&1 \
  | tail -1 | cut -c1-104 | sed 's/^/  stolen: /'
set -e

say "a repository outside allow, with a perfectly valid token"
set +e
ci "$mint repository=evil/payments" --count 1 --need linux --dry-run 2>&1 \
  | tail -1 | cut -c1-104 | sed 's/^/  /'
set -e

say "a tampered token — the signature is really checked, against keys really fetched"
set +e
ci "bash $HERE/forge.sh $LAB/issuer.pem {audience}" --count 1 --need linux --dry-run 2>&1 \
  | tail -1 | cut -c1-104 | sed 's/^/  /'
set -e

say "an unknown issuer"
set +e
ISS=https://issuer.test.evil.test ci "$mint repository=acme/payments" \
  --count 1 --need linux --dry-run 2>&1 | tail -1 | cut -c1-104 | sed 's/^/  /'
set -e

say "the audience is bound to the run, so the same token twice is still one caller"
# Not a replay test — a token is per-run by construction. What this shows is that the key
# changes every time, which is why a token cannot be reused even by its rightful owner.
for _ in 1 2; do
  ci "$mint repository=acme/payments" --count 1 --need linux --dry-run 2>&1 \
    | grep "attesting as" | cut -c1-70 | sed 's/^/  /'
done

say "named by host rather than by key — what a CI variable should hold"
# No CM_CONTROLLER_HINTS either: the document carries the addresses.
set +e
env -u CM_HOME -u SIRJI_HOME \
  CM_ATTEST_CMD="$mint repository=acme/payments" \
  "$CM" test "http://127.0.0.1:$JWKS_PORT" --count 1 --need linux --dry-run 2>&1 \
  | tail -3 | cut -c1-104 | sed 's/^/  /'
set -e

say "a bare word is none of the three ways to name one"
set +e
env -u CM_HOME -u SIRJI_HOME "$CM" test cm-c --count 1 --dry-run 2>&1 \
  | tail -1 | cut -c1-104 | sed 's/^/  /'
set -e

say "controller log"
grep -E "policy (weighed|refused)|attestation|issuer|not a tenant" cm-c.log \
  | cut -c1-104 | sed 's/^/  /' || true

say "done — lab in $LAB"
