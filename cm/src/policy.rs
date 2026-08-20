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

use anyhow::{Context, Result, bail};


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

    /// The two figures the fenced block puts on any answer: what this org calls ordinary,
    /// and the most interpretation may grant.
    ///
    /// Derived the same way `weigh` derives them, so a policy test is held to the numbers
    /// a real plea would be.
    pub fn bounds(&self) -> (u32, u32) {
        let standing = self.grants.standing_limit;
        (standing, self.grants.max_limit.unwrap_or(standing).max(standing))
    }

    pub fn reservation_secs(&self) -> u64 {
        self.grants.reservation_seconds
    }

    /// What this tenant allows itself, as (credits, window seconds).
    pub fn budget(&self) -> Option<(u64, u64)> {
        self.grants.daily_budget.map(|c| {
            (c, self.grants.budget_window.unwrap_or(crate::budget::WINDOW_SECS))
        })
    }

    pub fn may_ask(&self, asker: &str) -> bool {
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
    // Two blocks and only the first applying is the worst available outcome: somebody
    // appends a rule, sees no error, and believes it is in force. Which one was meant is
    // not cm's to guess.
    if text.matches("```yaml").count() > 1 {
        bail!(
            "more than one ```yaml block. Only the first would be applied, so the rest \
             would be rules that look enforced and are not — put them in one block."
        );
    }
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


    #[test]
    fn the_starter_policy_parses() {
        let p = policy(STARTER);
        assert!(p.may_ask("anyone-at-all"));
        assert_eq!(p.bounds(), (10, 10));
    }

    #[test]
    fn a_named_requester_list_excludes_others() {
        let p = policy("```yaml\nrequesters:\n  - dana\n  - kiran\nstanding_limit: 4\n```");
        assert!(p.may_ask("dana"));
        assert!(!p.may_ask("lee"));
    }

    #[test]
    fn a_missing_grants_block_still_works() {
        let p = policy("# policy.md\n\nJust prose, no fenced block.\n");
        assert!(p.may_ask("dana"));
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
        // Interpretation may still refuse or trim; raising a number takes an explicit
        // `max_limit`, and opting in is the organisation's act.
        assert_eq!(policy("```yaml\nstanding_limit: 4\n```").bounds(), (4, 4));
        assert_eq!(policy("```yaml\nstanding_limit: 4\nmax_limit: 20\n```").bounds(), (4, 20));
    }

    #[test]
    fn a_max_below_the_standing_limit_is_ignored_not_obeyed() {
        // It would otherwise put the ceiling *below* what this org calls an ordinary
        // request, leaving a model told that normal is 10 and it may grant at most 5.
        assert_eq!(policy("```yaml\nstanding_limit: 10\nmax_limit: 5\n```").bounds(), (10, 10));
    }

    #[test]
    fn a_second_grants_block_is_an_error_not_a_silent_omission() {
        // Found by a scenario that meant to break a policy and appended a second block
        // instead: it was accepted, because only the first is read. Somebody appending a
        // rule and seeing no error would believe it was in force.
        let two = "```yaml\nstanding_limit: 2\n```\n\nprose\n\n```yaml\nmax_limit: 9\n```";
        let e = parse_grants(two).unwrap_err().to_string();
        assert!(e.contains("more than one"), "{e}");
        // One is still fine, fenced examples in the prose included — those are not `yaml`.
        assert!(parse_grants("```yaml\nstanding_limit: 2\n```\n\n```sh\ncm t\n```").is_ok());
    }

    #[test]
    fn a_bad_limit_is_an_error_not_a_default() {
        // Silently falling back would give the org a limit it never wrote.
        assert!(parse_grants("```yaml\nstanding_limit: lots\n```").is_err());
    }
}
