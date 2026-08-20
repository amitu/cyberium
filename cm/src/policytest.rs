//! `cm policy-test` — checking a policy the way you check code.
//!
//! A policy decides how much money a fleet spends, and it is now decided by a model
//! reading prose. Both halves of that need a test: prose can be ambiguous in ways nobody
//! notices until a release night, and an edit meant to tighten one rule can loosen
//! another. So cases are checked in beside the policy, and this runs them:
//!
//! ```json
//! {
//!   "name": "a nightly run is held to the standing limit",
//!   "caller": "dana",
//!   "asked": 6,
//!   "said": { "plea": "nightly-regression" },
//!   "expect": { "at_most": 2 }
//! }
//! ```
//!
//! It needs no controller and no fleet — a folder, a model key, and the cases. That is
//! the point: it runs in the organisation's own CI, on the repository where the policy
//! lives, before anybody uploads anything.
//!
//! **The cases are not part of the policy.** They live in `policy-tests/`, which
//! [`crate::rulebook`] excludes, because a folder is sent to the model verbatim — and a
//! test that ships its own answer key inside the prompt tests nothing at all. That is the
//! one thing here that would be silently, completely broken if it were wrong.
//!
//! The decision comes from [`crate::weigh`], the same function the controller calls. Not
//! a reimplementation: a policy test that passes against a slightly different decision
//! than the fleet makes is worse than no policy test, because it is believed.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::controller::{Limit, weigh};
use crate::{adviser, fleet, rulebook};

pub const DIR: &str = "policy-tests";

/// One case. Everything but `expect` has a default, so a case says only what it is about.
#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub name: String,
    /// Who is asking, as the controller would have proven it.
    #[serde(default = "somebody")]
    pub caller: String,
    #[serde(default = "one")]
    pub asked: u32,
    #[serde(default)]
    pub need: Vec<String>,
    /// The keys and values the caller sent. cm reads none of them; the policy does.
    #[serde(default)]
    pub said: BTreeMap<String, String>,
    /// What the deployment would have attested about them — a plan, a group, a flag.
    ///
    /// Pinnable for the same reason `fleet` is: a rule that turns on a fact cannot be
    /// tested without stating the fact, and a case that left it out would be checking a
    /// different rule than the one it names.
    #[serde(default)]
    pub facts: BTreeMap<String, String>,
    /// The fleet at the moment of asking. Pinned, because the answer depends on it: "six
    /// machines" is a different question on a quiet Tuesday and a release night, and a
    /// case that did not say which would pass or fail by accident.
    #[serde(default)]
    pub fleet: FleetCase,
    #[serde(default)]
    pub money: Option<MoneyCase>,
    /// The host's cap on this tenant. Not in the tenant's own folder — it is the host's
    /// to set — so a case that cares has to say it.
    #[serde(default = "no_cap")]
    pub ceiling: u32,
    pub expect: Expect,
}

fn somebody() -> String {
    "somebody".into()
}
fn one() -> u32 {
    1
}
fn no_cap() -> u32 {
    u32::MAX
}

#[derive(Debug, Clone, Deserialize)]
pub struct FleetCase {
    pub capable: u32,
    pub free: u32,
    /// Credits per minute for the free ones, cheapest first.
    pub rates: Vec<u32>,
}

impl Default for FleetCase {
    /// A quiet fleet with room, so a case about a *rule* does not have to describe
    /// hardware to say what it means.
    fn default() -> Self {
        Self { capable: 32, free: 32, rates: vec![1; 32] }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct MoneyCase {
    pub budget: u64,
    #[serde(default)]
    pub spent: u64,
    #[serde(default)]
    pub committed: u64,
    #[serde(default = "day")]
    pub window: u64,
}

fn day() -> u64 {
    86_400
}

/// What the author claims should happen.
///
/// Four shapes, because a policy is not always precise and a test should be able to be as
/// vague as the rule it checks. "Countered back towards the standing limit" is a real
/// sentence to write, and `at_most` is how you check it without inventing a number the
/// policy never named.
#[derive(Debug, Clone, Deserialize)]
pub struct Expect {
    /// Exactly this many.
    #[serde(default)]
    pub count: Option<u32>,
    #[serde(default)]
    pub at_most: Option<u32>,
    #[serde(default)]
    pub at_least: Option<u32>,
    /// `allow`, `counter` or `deny`, as the caller would experience it.
    #[serde(default)]
    pub verdict: Option<String>,
}

impl Expect {
    /// Empty expectations would pass against anything, which is worse than no test: it
    /// looks like coverage.
    fn says_something(&self) -> bool {
        self.count.is_some()
            || self.at_most.is_some()
            || self.at_least.is_some()
            || self.verdict.is_some()
    }

    /// What went wrong, or nothing.
    fn check(&self, got: u32, asked: u32, denied: bool) -> Option<String> {
        let verdict = if denied || got == 0 {
            "deny"
        } else if got < asked {
            "counter"
        } else {
            "allow"
        };
        if let Some(want) = &self.verdict
            && !want.eq_ignore_ascii_case(verdict)
        {
            return Some(format!("expected {want}, got {verdict} ({got})"));
        }
        if let Some(want) = self.count
            && got != want
        {
            return Some(format!("expected {want}, got {got}"));
        }
        if let Some(most) = self.at_most
            && got > most
        {
            return Some(format!("expected at most {most}, got {got}"));
        }
        if let Some(least) = self.at_least
            && got < least
        {
            return Some(format!("expected at least {least}, got {got}"));
        }
        None
    }
}

/// Read every `.json` in `<dir>/policy-tests/`.
pub fn load(dir: &Path) -> Result<Vec<Case>> {
    let tests = dir.join(DIR);
    if !tests.is_dir() {
        bail!(
            "no {}/ in {} — a policy with no tests is a policy nobody can change safely",
            DIR,
            dir.display()
        );
    }
    let mut files: Vec<_> = std::fs::read_dir(&tests)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();

    let mut cases = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path)?;
        // One case or a list, because a file per case and a file of cases are both
        // reasonable ways to organise them and neither is worth arguing about.
        let parsed: Vec<Case> = if text.trim_start().starts_with('[') {
            serde_json::from_str(&text)
                .with_context(|| format!("reading {}", path.display()))?
        } else {
            vec![serde_json::from_str(&text)
                .with_context(|| format!("reading {}", path.display()))?]
        };
        for case in &parsed {
            if !case.expect.says_something() {
                bail!(
                    "{}: `{}` expects nothing, so it would pass against any answer",
                    path.display(),
                    case.name
                );
            }
        }
        cases.extend(parsed);
    }
    if cases.is_empty() {
        bail!("{}/ has no cases in it", tests.display());
    }
    Ok(cases)
}

/// Run every case against the folder, and say what happened.
///
/// `repeat` runs each case more than once. Worth having, and not paranoia: the answer
/// comes from a model, so "does this rule hold" and "does this rule hold *every time*"
/// are different questions, and only the second one tells you whether a policy is written
/// clearly enough to depend on.
pub async fn run(dir: &Path, repeat: u32, only: Option<&str>) -> Result<std::process::ExitCode> {
    let rulebook = rulebook::Rulebook::load(dir)?;
    if rulebook.as_str().trim().is_empty() {
        bail!("{} has nothing written down to test", dir.display());
    }
    let policy = crate::policy::Policy::load(dir)?;
    let adviser = adviser::Claude::from_env()?;
    // Filtered first, so the count is what will actually run. "6 cases" followed by one
    // result reads like five went missing.
    let cases: Vec<Case> = load(dir)?
        .into_iter()
        .filter(|c| only.is_none_or(|want| c.name.contains(want)))
        .collect();
    if cases.is_empty() {
        bail!("no case matches {:?}", only.unwrap_or_default());
    }

    let each = if repeat > 1 { format!(", {repeat} times each") } else { String::new() };
    println!("{} case(s) against {}{each}", cases.len(), dir.display());
    println!("weighed by: {}\n", adviser.describe());

    let mut failed = 0u32;
    for case in &cases {
        match one_case(&adviser, &rulebook, &policy, case, repeat).await {
            Ok(()) => println!("  ok    {}", case.name),
            Err(why) => {
                failed += 1;
                println!("  FAIL  {}\n          {why}", case.name);
            }
        }
    }

    if failed == 0 {
        println!("\nall good");
        return Ok(std::process::ExitCode::SUCCESS);
    }
    println!("\n{failed} case(s) failed");
    Ok(std::process::ExitCode::FAILURE)
}

async fn one_case(
    adviser: &adviser::Claude,
    rulebook: &rulebook::Rulebook,
    policy: &crate::policy::Policy,
    case: &Case,
    repeat: u32,
) -> std::result::Result<(), String> {
    let lifetime = policy.reservation_secs();
    let (standing, ceiling) = policy.bounds();
    let brief = fleet::Brief {
        fleet: case.fleet.capable,
        capable: case.fleet.capable,
        free: case.fleet.free,
        rates: case.fleet.rates.clone(),
    };
    let money = case.money.as_ref().map(|m| adviser::Money {
        budget: m.budget,
        spent: m.spent,
        committed: m.committed,
        window_secs: m.window,
    });
    let advice = adviser::Advice {
        rulebook: rulebook.as_str().to_string(),
        attested: adviser::Attested {
            // The tenant's own folder is under test, so the tenant is whatever it is; the
            // caller is what a policy actually distinguishes people by.
            tenant: "under-test".into(),
            caller: case.caller.clone(),
            facts: case.facts.clone(),
        },
        declared: adviser::Declared {
            said: case.said.clone(),
            count: case.asked,
            capabilities: case.need.clone(),
        },
        standing,
        ceiling: ceiling.min(case.ceiling),
        lifetime,
        fleet: brief.clone(),
        money: money.clone(),
    };
    let limit = Limit {
        ceiling: ceiling.min(case.ceiling),
        free: brief.free,
        host: case.ceiling,
        affordable: money.as_ref().map(|m| {
            crate::budget::Room { budget: m.budget, spent: m.spent, committed: m.committed }
                .affordable(&brief.rates, lifetime)
        }),
    };

    for attempt in 1..=repeat.max(1) {
        let weighed = weigh(adviser, &advice, limit.clone())
            .await
            .map_err(|e| format!("{e:#}"))?;
        let (got, denied) = (weighed.allowed, weighed.denied.is_some());
        if let Some(mismatch) = case.expect.check(got, case.asked, denied) {
            // The rationale, always. Without it an author knows the number was wrong and
            // nothing about which sentence of theirs produced it.
            let run = if repeat > 1 { format!(" (run {attempt} of {repeat})") } else { String::new() };
            return Err(format!("{mismatch}{run}\n          it said: {}", weighed.rationale));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect(json: &str) -> Expect {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn an_exact_count_is_checked_exactly() {
        let e = expect(r#"{"count": 2}"#);
        assert!(e.check(2, 6, false).is_none());
        assert_eq!(e.check(3, 6, false).unwrap(), "expected 2, got 3");
    }

    #[test]
    fn a_vague_rule_can_be_checked_vaguely() {
        // "Countered back towards the standing limit" is a real sentence to write, and
        // pinning an exact number the policy never named would make the test wrong rather
        // than strict.
        let e = expect(r#"{"at_most": 2}"#);
        assert!(e.check(1, 6, false).is_none());
        assert!(e.check(2, 6, false).is_none());
        assert!(e.check(3, 6, false).unwrap().contains("at most 2"));

        let e = expect(r#"{"at_least": 4}"#);
        assert!(e.check(4, 6, false).is_none());
        assert!(e.check(3, 6, false).unwrap().contains("at least 4"));
    }

    #[test]
    fn the_verdict_is_what_the_caller_would_experience() {
        // Not the model's word for it: fewer than asked is a counter however it happened,
        // and nothing at all is a denial whether the model refused or a clamp took it.
        let e = expect(r#"{"verdict": "counter"}"#);
        assert!(e.check(2, 6, false).is_none(), "fewer than asked");
        assert!(e.check(6, 6, false).is_some(), "all of it is an allow");

        let deny = expect(r#"{"verdict": "deny"}"#);
        assert!(deny.check(0, 6, false).is_none(), "clamped to nothing");
        assert!(deny.check(0, 6, true).is_none(), "or refused outright");

        let allow = expect(r#"{"verdict": "allow"}"#);
        assert!(allow.check(6, 6, false).is_none());
        assert!(allow.check(0, 6, true).is_some());
    }

    #[test]
    fn expectations_can_be_combined() {
        let e = expect(r#"{"verdict": "counter", "at_least": 2, "at_most": 4}"#);
        assert!(e.check(3, 9, false).is_none());
        assert!(e.check(1, 9, false).is_some());
        assert!(e.check(5, 9, false).is_some());
    }

    #[test]
    fn a_case_that_expects_nothing_is_refused_at_load() {
        // It would pass against any answer, which is worse than having no test, because it
        // looks like coverage.
        assert!(!expect(r#"{}"#).says_something());
    }

    #[test]
    fn a_case_only_has_to_say_what_it_is_about() {
        // Defaults exist so a case about a *rule* does not have to describe hardware.
        let case: Case = serde_json::from_str(
            r#"{"name": "n", "said": {"plea": "nightly"}, "expect": {"at_most": 2}}"#,
        )
        .unwrap();
        assert_eq!(case.asked, 1);
        assert_eq!(case.caller, "somebody");
        assert!(case.fleet.free > 0, "a quiet fleet with room");
        assert_eq!(case.ceiling, u32::MAX, "no host cap unless the case says one");
        assert!(case.money.is_none());
    }

    #[test]
    fn the_tests_are_not_part_of_the_policy() {
        // The one thing here that would be silently, completely broken if wrong: the
        // folder is sent to the model verbatim, so a case inside it would hand over the
        // answer key with the question.
        let root = crate::testing::scratch("policytest");
        std::fs::write(root.join("policy.md"), "Be reasonable.").unwrap();
        std::fs::create_dir_all(root.join(DIR)).unwrap();
        std::fs::write(
            root.join(DIR).join("cases.json"),
            r#"{"name":"n","expect":{"count":2}}"#,
        )
        .unwrap();

        let book = rulebook::Rulebook::load(&root).unwrap();
        assert!(book.as_str().contains("Be reasonable."));
        assert!(!book.as_str().contains("expect"), "{}", book.as_str());
        assert!(!book.as_str().contains("cases.json"), "not even in the tree");
        std::fs::remove_dir_all(&root).ok();
    }
}
