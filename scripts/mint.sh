#!/usr/bin/env bash
#
# Stands in for a CI platform's token endpoint: signs one RS256 JWT for a given audience.
#
#   mint.sh <key.pem> <audience> [claim=value]...
#
# Everything here is `openssl` and shell on purpose — a test issuer that needed a library
# would be one more thing to install before the scenario could run, and the point is to
# exercise cm's verifier against a real signature rather than to be a good JWT library.
set -euo pipefail

KEY=$1; AUD=$2; shift 2

# base64url, no padding, which is what JWTs use and what `base64` does not do.
b64() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }

ISS=${ISS:-https://issuer.test}
NOW=$(date +%s)

claims="\"iss\":\"$ISS\",\"aud\":\"$AUD\",\"iat\":$NOW,\"exp\":$((NOW + 300))"
for pair in "$@"; do
  claims="$claims,\"${pair%%=*}\":\"${pair#*=}\""
done

header=$(printf '{"alg":"RS256","typ":"JWT","kid":"test-1"}' | b64)
payload=$(printf '{%s}' "$claims" | b64)
signature=$(printf '%s.%s' "$header" "$payload" \
  | openssl dgst -sha256 -sign "$KEY" -binary | b64)

printf '%s.%s.%s\n' "$header" "$payload" "$signature"
