//! `policy.md` — the org's rules, in the org's own words.
//!
//! One file, hand-edited, `git`-able. No admin UI and no second store, because a
//! rule that lives in two places eventually disagrees with itself.
//!
//! It has two halves on purpose. The fenced `grants` block is read
//! **deterministically** and decides who may even ask — an unauthorised caller is
//! refused without a model ever being consulted, so the cheap gate stays cheap.
//! Everything else is prose, weighed by a model against the requester's actual
//! reason.
//!
//! The model half is not yet wired: this reads the grants and applies the standing
//! limit. That is deliberate sequencing — the transport, the identity and the
//! refusal paths are worth proving before anything non-deterministic joins in.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::proto::Nivedana;

pub const FILE: &str = "policy.md";

pub fn path_in(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// What ships when there is no policy yet. Small on purpose: rules, not prose.
const STARTER: &str = r#"# policy.md

Who may ask, and for how much. The fenced block below is read deterministically —
a caller outside `requesters` is refused before any model is consulted. Everything
after it is prose, weighed against the reason the caller gave.

```yaml
requesters:
  - everyone
standing_limit: 10
reservation_seconds: 600
```

## Standing budgets

Anyone may ask for up to the standing limit without justification. A grant is taken
back after `reservation_seconds` if nobody releases it — the backstop for a caller
that dies mid-run. Set it above your longest suite, or long runs will be cut off.

## Circumstantial override

If a request asserts a production incident and names an incident tracker URL,
allow up to 5x the standing limit for one hour, then re-evaluate.

For everything else, apply the standing budget.
"#;

/// What policy permits. Not a verdict: a verdict also depends on what is free.
#[derive(Debug, Clone)]
pub enum Ruling {
    Allow {
        count: u32,
        rationale: Option<String>,
    },
    Counter {
        count: u32,
        rationale: String,
    },
    Deny {
        rationale: String,
    },
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub path: PathBuf,
    /// The whole file, prose included. Read but not yet reasoned over: this is what
    /// goes to the model once that call is wired, and it is loaded now so a broken
    /// or unreadable policy fails at startup rather than under the first caller.
    #[allow(dead_code)]
    pub text: String,
    grants: Grants,
}

#[derive(Debug, Clone)]
struct Grants {
    /// Aliases that may ask at all. `everyone` admits any authenticated caller.
    requesters: Vec<String>,
    /// How many machines are given without argument.
    standing_limit: u32,
    /// How long a grant survives unreleased. An org rule, not a constant: it has to
    /// sit above the longest suite anyone runs here.
    reservation_seconds: u64,
}

impl Default for Grants {
    fn default() -> Self {
        Self {
            requesters: vec!["everyone".into()],
            standing_limit: 10,
            reservation_seconds: 600,
        }
    }
}

impl Policy {
    /// Read `policy.md`, writing the starter if none exists.
    pub fn load(root: &Path) -> Result<Self> {
        let path = path_in(root);
        if !path.exists() {
            std::fs::create_dir_all(root)?;
            std::fs::write(&path, STARTER)
                .with_context(|| format!("writing {}", path.display()))?;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let grants = parse_grants(&text)
            .with_context(|| format!("reading the grants block in {}", path.display()))?;
        Ok(Self { path, text, grants })
    }

    /// How many machines policy permits, if any.
    ///
    /// Policy decides *entitlement*; it never picks machines. What is actually
    /// free, and which of them can do the work, is the fleet's business — keeping
    /// those apart is what lets policy stay a text file.
    pub fn weigh(&self, asker: &str, nivedana: &Nivedana) -> Ruling {
        if !self.may_ask(asker) {
            // The deterministic gate: refused without spending a token.
            return Ruling::Deny {
                rationale: format!("{asker} is not in the requesters list"),
            };
        }

        let wanted = nivedana.count.unwrap_or(1);
        if wanted <= self.grants.standing_limit {
            return Ruling::Allow {
                count: wanted,
                rationale: Some(format!(
                    "within the standing limit of {}",
                    self.grants.standing_limit
                )),
            };
        }

        Ruling::Counter {
            count: self.grants.standing_limit,
            rationale: format!(
                "the standing limit is {}; weighing the reason against the prose \
                 needs the model call, which is not wired yet",
                self.grants.standing_limit
            ),
        }
    }

    /// How long a grant survives unreleased.
    pub fn reservation_secs(&self) -> u64 {
        self.grants.reservation_seconds
    }

    fn may_ask(&self, asker: &str) -> bool {
        self.grants
            .requesters
            .iter()
            .any(|r| r == "everyone" || r == asker)
    }
}

/// Pull the first fenced `yaml` block out of the markdown and read it.
///
/// Hand-parsed rather than pulling in a YAML crate: the block is two keys, and a
/// dependency to read two keys is a dependency to keep current forever.
fn parse_grants(text: &str) -> Result<Grants> {
    let Some(block) = fenced_block(text, "yaml") else {
        return Ok(Grants::default());
    };

    let mut grants = Grants {
        requesters: Vec::new(),
        ..Default::default()
    };
    let mut in_requesters = false;

    for line in block.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ") {
            if in_requesters {
                grants.requesters.push(rest.trim().to_string());
            }
            continue;
        }
        in_requesters = false;
        if let Some((key, value)) = trimmed.split_once(':') {
            let value = value.trim();
            match key.trim() {
                "requesters" => {
                    in_requesters = true;
                    if !value.is_empty() {
                        grants.requesters.push(value.to_string());
                    }
                }
                "standing_limit" => {
                    grants.standing_limit = value
                        .parse()
                        .with_context(|| format!("standing_limit: {value:?} is not a number"))?;
                }
                "reservation_seconds" => {
                    grants.reservation_seconds = value.parse().with_context(|| {
                        format!("reservation_seconds: {value:?} is not a number")
                    })?;
                }
                _ => {}
            }
        }
    }

    if grants.requesters.is_empty() {
        grants.requesters.push("everyone".into());
    }
    Ok(grants)
}

fn fenced_block<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("```{tag}");
    let start = text.find(&open)? + open.len();
    let rest = &text[start..];
    let end = rest.find("```")?;
    Some(&rest[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(text: &str) -> Policy {
        Policy {
            path: PathBuf::from("policy.md"),
            text: text.to_string(),
            grants: parse_grants(text).unwrap(),
        }
    }

    fn plea(count: Option<u32>) -> Nivedana {
        Nivedana {
            why: "because".into(),
            count,
            ..Default::default()
        }
    }

    #[test]
    fn the_starter_policy_parses() {
        let p = policy(STARTER);
        assert!(p.may_ask("anyone-at-all"));
        assert!(matches!(p.weigh("dana", &plea(Some(3))), Ruling::Allow { .. }));
    }

    #[test]
    fn a_named_requester_list_excludes_others() {
        let p = policy("```yaml\nrequesters:\n  - dana\n  - kiran\nstanding_limit: 4\n```");
        assert!(p.may_ask("dana"));
        assert!(!p.may_ask("lee"));

        let denied = p.weigh("lee", &plea(Some(1)));
        assert!(matches!(denied, Ruling::Deny { .. }), "{denied:?}");
    }

    #[test]
    fn over_the_limit_is_countered_not_denied() {
        let p = policy("```yaml\nrequesters:\n  - everyone\nstanding_limit: 4\n```");
        match p.weigh("dana", &plea(Some(50))) {
            Ruling::Counter { count, .. } => assert_eq!(count, 4),
            other => panic!("expected a counter, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_grants_block_still_works() {
        let p = policy("# policy.md\n\nJust prose, no fenced block.\n");
        assert!(p.may_ask("dana"));
        assert!(matches!(p.weigh("dana", &plea(Some(1))), Ruling::Allow { .. }));
    }

    #[test]
    fn the_org_sets_how_long_a_grant_lasts() {
        let p = policy("```yaml\nstanding_limit: 4\nreservation_seconds: 7200\n```");
        assert_eq!(p.reservation_secs(), 7200);
        // And an org that says nothing gets the default rather than no timeout.
        assert_eq!(policy("no block here").reservation_secs(), 600);
    }

    #[test]
    fn a_bad_limit_is_an_error_not_a_default() {
        // Silently falling back would give the org a limit it never wrote.
        assert!(parse_grants("```yaml\nstanding_limit: lots\n```").is_err());
    }

    #[test]
    fn no_count_asks_for_one() {
        let p = policy(STARTER);
        match p.weigh("dana", &plea(None)) {
            Ruling::Allow { count, .. } => assert_eq!(count, 1),
            other => panic!("expected an allow, got {other:?}"),
        }
    }
}
