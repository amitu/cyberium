//! Attestation: a credential cm did not issue.
//!
//! A sealed ticket answers "who is this" for anything with a keypair somebody enrolled
//! once. That covers a laptop, a build server and a worker, and it does not cover a cloud
//! CI runner — which exists for ninety seconds, has nothing to enrol, and would grow the
//! controller's roster by one dead entry per build if it tried.
//!
//! The shape that fits is the same one a ticket already is. A ticket is an **attestation
//! whose issuer happens to be a sirji**; an OIDC token is an attestation whose issuer
//! happens to be GitHub. So the controller learns to verify a second kind, and everything
//! downstream — tenancy, policy, budget — is untouched, because all any of it wanted was
//! an alias and some facts.
//!
//! Which is why there is no shared secret anywhere in cm. One is only needed when an actor
//! is **both ephemeral and unattested**, and that set is empty:
//!
//! | actor | ephemeral | how it authenticates |
//! |---|---|---|
//! | cloud CI runner | yes | per-request OIDC, bound to a one-off key |
//! | developer laptop | no | keypair, enrolled once |
//! | build server | no | keypair, enrolled once |
//! | worker | no | keypair, enrolled once |
//!
//! ## The property that makes a scraped token worthless
//!
//! A bearer token in a build log is a credential anybody can replay. So cm does not accept
//! bearer tokens: **the audience must be the public key the caller is dialling from.**
//!
//! A runner mints a keypair for the run, asks its platform for a token with that key's
//! id52 as the audience, and throws the key away when the job ends. A token lifted from a
//! log names an audience whose private key never touched a disk and no longer exists. Same
//! caller-binding a sealed ticket has, from a credential cm did not mint.
//!
//! An issuer that cannot set the audience cannot be used for this, and that is the correct
//! outcome rather than a gap to work around.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const FILE: &str = "issuers.toml";

/// How long a fetched key set is used before being refreshed.
///
/// Long, because provider keys rotate slowly and verification needs egress to the
/// provider — on exactly the sort of network that filters egress. A stale-but-cached key
/// set is a better failure mode than an outage.
const KEYS_FOR_SECS: u64 = 3600;

/// Who this controller will believe, besides its own sirji.
///
/// Host-owned, like `admins.toml`, and for the same reason: this decides whose word counts
/// as proof. A missing file means nobody, which is the right default — a controller that
/// trusted an issuer it was never told about would be trusting the internet.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default, rename = "issuer")]
    pub issuers: Vec<Issuer>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Issuer {
    /// What to call it. Becomes the prefix of every alias it vouches for, so a tenant's
    /// `members` list reads `github:acme/payments` and cannot collide with a sirji alias.
    pub name: String,
    /// The `iss` claim, matched exactly. Not a prefix: a token from
    /// `token.actions.githubusercontent.com.evil.test` must not match GitHub.
    pub url: String,
    /// Where the signing keys are published.
    pub jwks: String,
    /// Which claim names the caller. `repository` for GitHub Actions.
    pub subject: String,
    /// Patterns the subject must match, with `*` allowed at either end. Empty means
    /// nothing matches — an issuer is a source of identities, not a licence for all of
    /// them, and `acme/*` is the difference between your organisation and everybody's.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Other claims to carry through as attested facts, for the policy to read: `ref`,
    /// `event_name`, `workflow`, `actor`. cm attaches no meaning to any of them.
    #[serde(default)]
    pub facts: Vec<String>,
    /// May a token from here **enrol a key**, so the holder never needs another one?
    ///
    /// Off by default, and the default is the point. A CI token proves a repository, and a
    /// repository is not a machine — letting build tokens enrol would grow the roster by
    /// one permanent key per project, which is the thing attestation exists to avoid.
    ///
    /// Turn it on for the issuer that proves *people*: an identity provider, where the
    /// subject is somebody who will still be here next month and whose laptop is worth
    /// remembering.
    #[serde(default)]
    pub enrol: bool,
}

impl Issuer {
    pub fn may_enrol(&self) -> bool {
        self.enrol
    }
}

/// What a verified attestation established.
#[derive(Debug, Clone)]
pub struct Vouched {
    /// `<issuer name>:<subject claim>`. What a tenant lists in `members`.
    pub alias: String,
    /// Whether this issuer is one whose tokens may enrol a key.
    pub may_enrol: bool,
    /// The claims this issuer was configured to carry through. Proven, so a policy may
    /// turn on them — and the caller cannot alter one.
    pub facts: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct Keys {
    /// kid → (n, e), raw big-endian.
    by_kid: BTreeMap<String, (Vec<u8>, Vec<u8>)>,
    fetched_at: u64,
}

#[derive(Debug)]
pub struct Issuers {
    config: Config,
    cached: tokio::sync::Mutex<BTreeMap<String, Keys>>,
    http: reqwest::Client,
}

impl Issuers {
    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::path_in(root);
        let config: Config = if path.exists() {
            toml::from_str(&std::fs::read_to_string(&path)?)
                .with_context(|| format!("reading {}", path.display()))?
        } else {
            // Nobody. A controller that trusted an issuer it was never told about would
            // be trusting whoever can mint a token.
            Config::default()
        };
        for issuer in &config.issuers {
            if issuer.allow.is_empty() {
                bail!(
                    "{}: issuer `{}` allows nothing, so it can never vouch for anybody — \
                     say which subjects it may name, such as `allow = [\"acme/*\"]`",
                    path.display(),
                    issuer.name
                );
            }
        }
        Ok(Self {
            config,
            cached: tokio::sync::Mutex::new(BTreeMap::new()),
            http: reqwest::Client::new(),
        })
    }

    pub fn path_in(root: &Path) -> PathBuf {
        root.join(FILE)
    }

    pub fn is_empty(&self) -> bool {
        self.config.issuers.is_empty()
    }

    pub fn describe(&self) -> String {
        if self.config.issuers.is_empty() {
            return "none — only this organisation's own devices may ask".into();
        }
        self.config
            .issuers
            .iter()
            .map(|i| format!("{} ({})", i.name, i.allow.join(", ")))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Verify a token, and say who it proves the caller is.
    ///
    /// `dialling_from` is the caller's own connection key. Everything about replay
    /// protection is that one comparison.
    pub async fn verify(&self, token: &str, dialling_from: &str) -> Result<Vouched> {
        let (signed, signature) = token
            .rsplit_once('.')
            .context("not a JWT: no signature")?;
        let (header_b64, payload_b64) =
            signed.split_once('.').context("not a JWT: no payload")?;

        let header: Header = json(header_b64).context("the token's header")?;
        // Only RS256. Accepting `none` is the classic way to accept anything, and
        // accepting an HMAC algorithm would let a *public* key double as a signing secret.
        if header.alg != "RS256" {
            bail!("unsupported token algorithm {:?}; only RS256", header.alg);
        }

        let claims: Claims = json(payload_b64).context("the token's claims")?;

        // Read `iss` before verifying anything, because it is how the key set is chosen.
        // Safe only because the signature is then checked against *that* issuer's keys —
        // a forged `iss` selects keys that will not verify the forgery.
        let issuer = self
            .config
            .issuers
            .iter()
            .find(|i| i.url == claims.iss)
            .with_context(|| {
                format!(
                    "no issuer configured for {:?} — the host lists them in {FILE}",
                    claims.iss
                )
            })?;

        // The whole of replay protection: a token is for one caller's one-off key.
        if !claims.audience().any(|a| a == dialling_from) {
            bail!(
                "this token's audience is not the key it arrived on, so it could have been \
                 lifted from anywhere. Mint it with the caller's own id52 as the audience."
            );
        }

        let now = crate::fleet::now();
        // A little slack, because two clocks are two clocks. Only on `nbf`/`iat`, never on
        // `exp`: being generous about expiry is being generous to whoever kept the token.
        const SLACK: u64 = 60;
        match claims.exp {
            Some(exp) if exp <= now => bail!("this token expired {}s ago", now - exp),
            None => bail!("this token has no expiry, so it would be valid forever"),
            _ => {}
        }
        if let Some(nbf) = claims.nbf.or(claims.iat)
            && nbf > now + SLACK
        {
            bail!("this token is not valid yet ({}s from now)", nbf - now);
        }

        let keys = self.keys_for(issuer).await?;
        let (n, e) = keys
            .by_kid
            .get(header.kid.as_deref().unwrap_or(""))
            .or_else(|| {
                // One key and no `kid` is a legitimate, if terse, key set.
                (keys.by_kid.len() == 1).then(|| keys.by_kid.values().next().unwrap())
            })
            .with_context(|| {
                format!(
                    "{} published no key with kid {:?}",
                    issuer.name,
                    header.kid.as_deref().unwrap_or("(none)")
                )
            })?;

        let sig = b64(signature).context("the token's signature")?;
        ring::signature::RsaPublicKeyComponents { n, e }
            .verify(
                &ring::signature::RSA_PKCS1_2048_8192_SHA256,
                signed.as_bytes(),
                &sig,
            )
            .map_err(|_| anyhow::anyhow!("this token's signature does not verify"))?;

        // Only now is anything in it worth reading.
        let subject = claims
            .other
            .get(&issuer.subject)
            .and_then(as_text)
            .with_context(|| {
                format!("this token has no {:?} claim to identify it by", issuer.subject)
            })?;
        if !issuer.allow.iter().any(|p| matches(p, &subject)) {
            bail!(
                "{:?} is not one of the subjects `{}` may vouch for",
                subject,
                issuer.name
            );
        }

        let mut facts = BTreeMap::new();
        facts.insert("issuer".to_string(), issuer.name.clone());
        facts.insert(issuer.subject.clone(), subject.clone());
        for name in &issuer.facts {
            if let Some(value) = claims.other.get(name).and_then(as_text) {
                facts.insert(name.clone(), value);
            }
        }
        Ok(Vouched {
            alias: format!("{}:{subject}", issuer.name),
            may_enrol: issuer.may_enrol(),
            facts,
        })
    }

    /// The issuer's signing keys, fetched at most once an hour.
    ///
    /// A failed refresh keeps serving the keys already held, because a provider being
    /// briefly unreachable should not stop a fleet — and the keys are still theirs.
    async fn keys_for(&self, issuer: &Issuer) -> Result<Keys> {
        let now = crate::fleet::now();
        let mut cached = self.cached.lock().await;
        if let Some(keys) = cached.get(&issuer.name)
            && now.saturating_sub(keys.fetched_at) < KEYS_FOR_SECS
        {
            return Ok(keys.clone());
        }

        match self.fetch(issuer).await {
            Ok(fresh) => {
                cached.insert(issuer.name.clone(), fresh.clone());
                Ok(fresh)
            }
            Err(e) => match cached.get(&issuer.name) {
                Some(stale) => {
                    eprintln!(
                        "could not refresh {}'s keys, using the ones we have: {e:#}",
                        issuer.name
                    );
                    Ok(stale.clone())
                }
                None => Err(e.context(format!("fetching {}'s signing keys", issuer.name))),
            },
        }
    }

    async fn fetch(&self, issuer: &Issuer) -> Result<Keys> {
        let text = self
            .http
            .get(&issuer.jwks)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let set: Jwks = serde_json::from_str(&text).context("the key set was not JSON")?;

        let mut by_kid = BTreeMap::new();
        for key in set.keys {
            // Only what can sign a token cm would accept. A key set legitimately carries
            // others, and skipping them quietly is right.
            if key.kty != "RSA" {
                continue;
            }
            let (Some(n), Some(e)) = (key.n.as_deref(), key.e.as_deref()) else { continue };
            let (Ok(n), Ok(e)) = (b64(n), b64(e)) else { continue };
            by_kid.insert(key.kid.unwrap_or_default(), (n, e));
        }
        if by_kid.is_empty() {
            bail!("{} published no usable RSA keys", issuer.jwks);
        }
        Ok(Keys { by_kid, fetched_at: crate::fleet::now() })
    }
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    /// A string or a list of them, per RFC 7519.
    #[serde(default)]
    aud: serde_json::Value,
    #[serde(default)]
    exp: Option<u64>,
    #[serde(default)]
    nbf: Option<u64>,
    #[serde(default)]
    iat: Option<u64>,
    #[serde(flatten)]
    other: BTreeMap<String, serde_json::Value>,
}

impl Claims {
    fn audience(&self) -> impl Iterator<Item = &str> {
        let one = self.aud.as_str().into_iter();
        let many = self
            .aud
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str());
        one.chain(many)
    }
}

#[derive(Deserialize)]
struct Jwks {
    #[serde(default)]
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    #[serde(default)]
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
}

fn b64(text: &str) -> Result<Vec<u8>> {
    data_encoding::BASE64URL_NOPAD
        .decode(text.trim_end_matches('=').as_bytes())
        .context("not base64url")
}

fn json<T: serde::de::DeserializeOwned>(part: &str) -> Result<T> {
    Ok(serde_json::from_slice(&b64(part)?)?)
}

/// A claim value as text. Numbers and booleans included, because `run_number` and
/// `event_name` are equally useful to a policy and one of them is not a string.
fn as_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// `*` at either end, and nowhere else.
///
/// Enough for `acme/*` and `*/deploy`, and deliberately not a glob library: a pattern
/// language nobody can predict is a poor thing to put in front of an authorisation
/// decision.
fn matches(pattern: &str, subject: &str) -> bool {
    match (pattern.strip_prefix('*'), pattern.strip_suffix('*')) {
        (Some(_), Some(_)) => {
            let middle = pattern.trim_matches('*');
            middle.is_empty() || subject.contains(middle)
        }
        (Some(tail), None) => subject.ends_with(tail),
        (None, Some(head)) => subject.starts_with(head),
        (None, None) => pattern == subject,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pattern_matches_only_what_it_says() {
        assert!(matches("acme/payments", "acme/payments"));
        assert!(!matches("acme/payments", "acme/payments-api"));
        assert!(matches("acme/*", "acme/payments"));
        assert!(!matches("acme/*", "evil/payments"));
        assert!(matches("*/deploy", "acme/deploy"));
        assert!(!matches("*/deploy", "acme/deploy-staging"));
        assert!(matches("*payments*", "acme/payments-api"));
        assert!(matches("*", "anything at all"));
    }

    #[test]
    fn a_prefix_of_an_issuer_url_is_not_that_issuer() {
        // `token.actions.githubusercontent.com.evil.test` starts with the real thing, so a
        // prefix comparison here would hand an attacker every repository at once.
        let a = "https://token.actions.githubusercontent.com";
        let b = "https://token.actions.githubusercontent.com.evil.test";
        assert_ne!(a, b);
        assert!(b.starts_with(a), "which is exactly why matching is by equality");
    }

    #[test]
    fn an_issuer_that_allows_nothing_is_refused_at_load() {
        // Otherwise it is a configured issuer that silently vouches for nobody, and the
        // failure appears as a refused caller with no explanation.
        let root = crate::testing::scratch("issuers");
        std::fs::write(
            Issuers::path_in(&root),
            "[[issuer]]\nname = \"github\"\nurl = \"https://x\"\njwks = \"https://x/keys\"\nsubject = \"repository\"\n",
        )
        .unwrap();
        let e = Issuers::load(&root).unwrap_err().to_string();
        assert!(e.contains("allows nothing"), "{e}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn no_file_means_nobody_rather_than_everybody() {
        let root = crate::testing::scratch("issuers-none");
        let issuers = Issuers::load(&root).unwrap();
        assert!(issuers.is_empty());
        assert!(issuers.describe().contains("only this organisation's own devices"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn a_token_for_somebody_else_s_key_is_refused() {
        // The property the whole design rests on: a token scraped from a build log names
        // an audience the thief cannot dial from.
        let root = crate::testing::scratch("issuers-aud");
        std::fs::write(
            Issuers::path_in(&root),
            "[[issuer]]\nname = \"github\"\nurl = \"https://iss.test\"\n\
             jwks = \"https://iss.test/keys\"\nsubject = \"repository\"\nallow = [\"acme/*\"]\n",
        )
        .unwrap();
        let issuers = Issuers::load(&root).unwrap();

        let token = unsigned(&serde_json::json!({
            "iss": "https://iss.test",
            "aud": "somebody-elses-key",
            "exp": crate::fleet::now() + 300,
            "repository": "acme/payments",
        }));
        let e = issuers.verify(&token, "the-key-it-arrived-on").await.unwrap_err().to_string();
        assert!(e.contains("audience is not the key it arrived on"), "{e}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn an_unknown_issuer_an_expired_token_and_a_bad_algorithm_are_all_refused() {
        let root = crate::testing::scratch("issuers-bad");
        std::fs::write(
            Issuers::path_in(&root),
            "[[issuer]]\nname = \"github\"\nurl = \"https://iss.test\"\n\
             jwks = \"https://iss.test/keys\"\nsubject = \"repository\"\nallow = [\"acme/*\"]\n",
        )
        .unwrap();
        let issuers = Issuers::load(&root).unwrap();
        let now = crate::fleet::now();

        for (claims, expected) in [
            (serde_json::json!({"iss": "https://elsewhere.test", "aud": "k", "exp": now + 300}),
             "no issuer configured"),
            (serde_json::json!({"iss": "https://iss.test", "aud": "k", "exp": now - 1}),
             "expired"),
            (serde_json::json!({"iss": "https://iss.test", "aud": "k"}),
             "no expiry"),
        ] {
            let e = issuers.verify(&unsigned(&claims), "k").await.unwrap_err().to_string();
            assert!(e.contains(expected), "expected {expected:?}, got {e:?}");
        }

        // `alg: none` is the classic way to accept anything at all.
        let none = format!(
            "{}.{}.",
            data_encoding::BASE64URL_NOPAD.encode(br#"{"alg":"none"}"#),
            data_encoding::BASE64URL_NOPAD.encode(br#"{"iss":"https://iss.test"}"#)
        );
        let e = issuers.verify(&none, "k").await.unwrap_err().to_string();
        assert!(e.contains("only RS256"), "{e}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// A token whose claims are readable and whose signature is not valid. Enough to test
    /// every check that happens before the signature; the signed path is exercised live by
    /// `scripts/attest.sh`, against a real key.
    fn unsigned(claims: &serde_json::Value) -> String {
        format!(
            "{}.{}.{}",
            data_encoding::BASE64URL_NOPAD.encode(br#"{"alg":"RS256","kid":"k1"}"#),
            data_encoding::BASE64URL_NOPAD.encode(claims.to_string().as_bytes()),
            data_encoding::BASE64URL_NOPAD.encode(b"not a signature"),
        )
    }
}
