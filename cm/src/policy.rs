//! `policy.md` — the org's rules, in the org's own words.
//!
//! One file, hand-edited, `git`-able. No admin UI and no second store, because a
//! rule that lives in two places eventually disagrees with itself.
//!
//! It has two halves on purpose. The fenced `grants` block is read
//! **deterministically** and settles the one question that needs no interpretation:
//! who may ask at all. An unauthorised caller is refused without a model ever being
//! consulted.
//!
//! Everything else is prose, and the prose is where the number comes from — every
//! plea, not only the large ones. The block's limits bound that answer rather than
//! pre-empting it: `max_limit` is a ceiling interpretation may not exceed, and
//! `standing_limit` is what this org calls an ordinary request — sent to the model so
//! it has some idea what "normal" looks like here, since one told only a ceiling
//! drifts toward the ceiling. Answering the ordinary case from the block alone would
//! leave a policy that only spoke about exceptions.

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
a caller outside `requesters` is refused before any model is consulted, and no
answer ever exceeds the limits set here. Everything after it is prose, and the prose
is what decides how many machines a request deserves.

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
    Deny {
        rationale: String,
    },
    /// The plea reached the prose, which is where the number comes from.
    ///
    /// Returned rather than resolved here so `policy.rs` stays synchronous and pure.
    /// The model call needs a runtime, a network, a key and a timeout, and all four
    /// already live at the controller — putting them here would make every test of
    /// the cheap gate drag them along.
    Consider {
        /// What the caller asked for. An upper bound on the answer: nobody is given
        /// machines they did not ask for.
        wanted: u32,
        /// What this organisation calls an ordinary request. Calibration for the
        /// model, and nothing else: not a floor, not a fast path, and not a fallback —
        /// a plea that cannot be weighed fails rather than quietly landing here.
        standing: u32,
        /// The most any interpretation may grant. **Never** exceeded, whatever the
        /// model says.
        ceiling: u32,
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
    /// The most the **prose** may grant, above the standing limit.
    ///
    /// Absent means prose cannot expand entitlement at all, and that is the default
    /// on purpose: a model should only be able to raise a number an organisation has
    /// explicitly written down. Opt in, in your own file, to your own ceiling.
    max_limit: Option<u32>,
    /// Credits this tenant allows itself per rolling window. Inside whatever the host
    /// allows, never beyond it.
    daily_budget: Option<u64>,
    /// Seconds the budget is counted over. Rolling, so no timezone is involved; a
    /// named calendar is a thing the prose half will express once a model reads it.
    budget_window: Option<u64>,
}

impl Default for Grants {
    fn default() -> Self {
        Self {
            requesters: vec!["everyone".into()],
            standing_limit: 10,
            reservation_seconds: 600,
            max_limit: None,
            daily_budget: None,
            budget_window: None,
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
            // The one thing that needs no interpretation: whether this caller may
            // ask at all. Refused without spending a token.
            return Ruling::Deny {
                rationale: format!("{asker} is not in the requesters list"),
            };
        }

        // Everything else is the prose's to decide. How many machines a plea deserves
        // is exactly the question the policy was written to answer, so a number
        // arrived at without reading it would not be this organisation's answer — it
        // would be a default wearing its name.
        //
        // A `max_limit` below the standing limit is a contradiction in the file, not
        // a tighter rule: the standing limit is the firmer promise, so it wins.
        let standing = self.grants.standing_limit;
        Ruling::Consider {
            wanted: nivedana.count.unwrap_or(1),
            standing,
            // Absent `max_limit`, interpretation can lower a number but never raise
            // one. That is the default on purpose: a model should only be able to
            // exceed the standing limit where somebody wrote down how far.
            ceiling: self.grants.max_limit.unwrap_or(standing).max(standing),
        }
    }


    /// How long a grant survives unreleased.
    pub fn reservation_secs(&self) -> u64 {
        self.grants.reservation_seconds
    }

    /// What this tenant allows itself, as (credits, window seconds).
    pub fn budget(&self) -> Option<(u64, u64)> {
        self.grants.daily_budget.map(|c| {
            (c, self.grants.budget_window.unwrap_or(crate::budget::WINDOW_SECS))
        })
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
                "daily_budget" => {
                    grants.daily_budget = Some(value.parse().with_context(|| {
                        format!("daily_budget: {value:?} is not a number of credits")
                    })?);
                }
                "budget_window" => {
                    grants.budget_window = Some(value.parse().with_context(|| {
                        format!("budget_window: {value:?} is not a number of seconds")
                    })?);
                }
                "max_limit" => {
                    grants.max_limit = Some(value.parse().with_context(|| {
                        format!("max_limit: {value:?} is not a number")
                    })?);
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
            count,
            ..Default::default()
        }
    }

    #[test]
    fn the_starter_policy_parses() {
        let p = policy(STARTER);
        assert!(p.may_ask("anyone-at-all"));
        assert!(matches!(p.weigh("dana", &plea(Some(3))), Ruling::Consider { .. }));
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
    fn asking_for_a_lot_is_a_question_for_the_prose_not_a_refusal() {
        // The deterministic half does not counter, because it does not have a number
        // to counter with — 4 is what this org calls ordinary, not what it decided.
        let p = policy("```yaml\nrequesters:\n  - everyone\nstanding_limit: 4\n```");
        match p.weigh("dana", &plea(Some(50))) {
            Ruling::Consider { wanted, standing, ceiling } => {
                assert_eq!((wanted, standing, ceiling), (50, 4, 4));
            }
            other => panic!("expected Consider, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_grants_block_still_works() {
        let p = policy("# policy.md\n\nJust prose, no fenced block.\n");
        assert!(p.may_ask("dana"));
        assert!(matches!(p.weigh("dana", &plea(Some(1))), Ruling::Consider { .. }));
    }

    #[test]
    fn the_org_sets_how_long_a_grant_lasts() {
        let p = policy("```yaml\nstanding_limit: 4\nreservation_seconds: 7200\n```");
        assert_eq!(p.reservation_secs(), 7200);
        // And an org that says nothing gets the default rather than no timeout.
        assert_eq!(policy("no block here").reservation_secs(), 600);
    }

    #[test]
    fn prose_cannot_expand_entitlement_unless_the_org_said_so() {
        // The prose is still read and still decides — it simply cannot go above the
        // standing limit. Interpretation may lower a number; raising one takes an
        // explicit `max_limit`, and opting in is the organisation's act.
        let p = policy("```yaml\nstanding_limit: 4\n```");
        match p.weigh("dana", &plea(Some(50))) {
            Ruling::Consider { standing, ceiling, .. } => assert_eq!((standing, ceiling), (4, 4)),
            other => panic!("expected Consider, got {other:?}"),
        }
    }

    #[test]
    fn the_prose_decides_every_plea_not_only_the_large_ones() {
        // The whole proposition is that a policy written in English is what allocates
        // machines. A cheap path that answered small pleas without reading it would
        // make the policy an exception handler.
        let p = policy("```yaml\nstanding_limit: 4\nmax_limit: 20\n```");
        for asked in [1, 3, 4, 12, 50] {
            match p.weigh("dana", &plea(Some(asked))) {
                Ruling::Consider { wanted, standing, ceiling } => {
                    assert_eq!((wanted, standing, ceiling), (asked, 4, 20));
                }
                other => panic!("expected Consider for {asked}, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_max_below_the_standing_limit_is_ignored_not_obeyed() {
        // It would otherwise put the ceiling *below* what this org calls an ordinary
        // request, which is not what a maximum means — and would leave a model told
        // that normal is 10 and it may grant at most 5.
        let p = policy("```yaml\nstanding_limit: 10\nmax_limit: 5\n```");
        match p.weigh("dana", &plea(Some(50))) {
            Ruling::Consider { standing, ceiling, .. } => assert_eq!((standing, ceiling), (10, 10)),
            other => panic!("expected Consider, got {other:?}"),
        }
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
            Ruling::Consider { wanted, .. } => assert_eq!(wanted, 1),
            other => panic!("expected Consider, got {other:?}"),
        }
    }
}
