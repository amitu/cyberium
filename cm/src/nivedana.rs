//! `nivedanas/` — the pleas an organisation is willing to hear, in its own words.
//!
//! A nivedana (निवेदन, a plea) is the reason a caller wants machines. Left as free
//! text it is the one part of the prompt written by whoever is asking, which puts an
//! untrusted string on the input path of every allocation now that every plea is
//! weighed. Labelling it as data helps and is not a guarantee.
//!
//! So an organisation can write the pleas down instead. Each heading in any `.md` file
//! here is an alias, and the prose under it is what the model actually reads:
//!
//! ```markdown
//! ## nightly regression
//!
//! The scheduled full-suite run. Routine, predictable, and never urgent — it has all
//! night. Prefer the cheapest machines and do not exceed the standing limit.
//!
//! ## production incident
//!
//! An engineer is debugging a live outage and needs the suite bisected quickly. Worth
//! the maximum and worth the money, but check the context names an incident.
//! ```
//!
//! A caller then names one — `cm t … --plea nightly-regression` — and the words the
//! model weighs are the organisation's, not theirs. **The whole catalogue goes into the
//! cached half of the prompt** and the caller's message carries only which alias they
//! picked, so what they contribute is an index into an org-authored list rather than
//! prose. Policy can then talk about pleas by name, because the model has seen them
//! all.
//!
//! Once a tenant defines any plea, free text stops being accepted from that tenant —
//! see [`Nivedanas::is_empty`]. Defining one is how an organisation turns the guarantee
//! on, and doing it by writing a file is deliberate: no flag to forget.
//!
//! Anything a caller genuinely needs to *add* travels as the nivedana's context JSON,
//! which stays in the data section, where a policy can require a field without any of
//! it becoming instructions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

pub const DIR: &str = "nivedanas";

/// The pleas one tenant will hear, by alias.
#[derive(Debug, Clone, Default)]
pub struct Nivedanas {
    /// Normalised alias → the org's own words. `BTreeMap` so the catalogue reaches the
    /// prompt in the same order every time: a set that reshuffled itself would break
    /// the cache prefix for no reason.
    by_alias: BTreeMap<String, Plea>,
}

#[derive(Debug, Clone)]
pub struct Plea {
    /// The heading as written, for showing people.
    pub title: String,
    /// The prose under it. What the model reads.
    pub prose: String,
}

impl Nivedanas {
    /// Read every `.md` file in `<root>/nivedanas/`. A missing directory means none.
    ///
    /// Several files on purpose: a big organisation splits its pleas the way it splits
    /// anything else, and the alternative is one file nobody wants to edit.
    pub fn load(root: &Path) -> Result<Self> {
        let dir = root.join(DIR);
        if !dir.is_dir() {
            return Ok(Self::default());
        }
        // Sorted, so two controllers reading the same directory build the same prompt.
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        files.sort();

        let mut by_alias: BTreeMap<String, Plea> = BTreeMap::new();
        for path in files {
            let text = std::fs::read_to_string(&path)?;
            for plea in parse(&text) {
                let key = alias_of(&plea.title);
                if key.is_empty() {
                    bail!("{}: a heading with no name", path.display());
                }
                // Same rule as two tenants claiming one caller: a collision is refused
                // rather than resolved, because whichever way it resolved would be a
                // silent choice about whose plea a caller gets.
                if let Some(first) = by_alias.get(&key) {
                    bail!(
                        "{}: `{}` and `{}` are the same plea (`{key}`)",
                        path.display(),
                        first.title,
                        plea.title
                    );
                }
                by_alias.insert(key, plea);
            }
        }
        Ok(Self { by_alias })
    }

    /// No pleas written down, so free text is still accepted from this tenant.
    pub fn is_empty(&self) -> bool {
        self.by_alias.is_empty()
    }

    pub fn get(&self, asked: &str) -> Option<&Plea> {
        self.by_alias.get(&alias_of(asked))
    }

    /// For the refusal message. Somebody who named the wrong plea needs the list, not
    /// the news that they were wrong.
    pub fn aliases(&self) -> Vec<&str> {
        self.by_alias.keys().map(String::as_str).collect()
    }

    /// The catalogue as the model sees it: every plea, in a stable order.
    ///
    /// All of them, not the one that was chosen, because this half of the prompt is
    /// cached and must not vary with the request. It also lets a policy name a plea and
    /// be understood.
    pub fn catalogue(&self) -> String {
        self.by_alias
            .iter()
            .map(|(key, plea)| format!("### {key}\n{}\n", plea.prose))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Lowercase, and every run of anything else becomes one hyphen.
///
/// Applied to the heading *and* to what a caller typed, so `## Nightly Regression` is
/// reachable as `nightly-regression`, `"Nightly Regression"` or `nightly_regression`.
/// Nothing is renamed — the heading is still shown as written; only the lookup is
/// forgiving, because a plea nobody can spell is a plea nobody uses.
pub fn alias_of(heading: &str) -> String {
    let mut out = String::with_capacity(heading.len());
    let mut pending = false;
    for c in heading.chars() {
        if c.is_alphanumeric() {
            if pending && !out.is_empty() {
                out.push('-');
            }
            pending = false;
            out.extend(c.to_lowercase());
        } else {
            pending = true;
        }
    }
    out
}

/// Split markdown into headings and the prose under each.
fn parse(text: &str) -> Vec<Plea> {
    let mut out: Vec<Plea> = Vec::new();
    let mut title: Option<String> = None;
    let mut body = String::new();
    let mut fenced = false;

    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        // A `#` inside a fence is a shell comment, not a heading.
        if !fenced && line.starts_with('#') {
            if let Some(t) = title.take() {
                out.push(Plea { title: t, prose: body.trim().to_string() });
            }
            title = Some(line.trim_start_matches('#').trim().to_string());
            body = String::new();
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some(t) = title {
        out.push(Plea { title: t, prose: body.trim().to_string() });
    }
    // A heading with nothing under it says nothing to the model, and a caller naming it
    // would get a decision made on no information at all.
    out.into_iter().filter(|p| !p.prose.is_empty()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = crate::testing::scratch("nivedana");
        std::fs::create_dir_all(dir.join(DIR)).unwrap();
        dir
    }

    fn write(root: &Path, name: &str, text: &str) {
        std::fs::write(root.join(DIR).join(name), text).unwrap();
    }

    #[test]
    fn no_directory_means_no_pleas_not_an_error() {
        // The zero-config path: a tenant that has written none still works, and free
        // text is still heard from them.
        let root = crate::testing::scratch("niv-none");
        assert!(Nivedanas::load(&root).unwrap().is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_heading_is_an_alias_and_the_prose_under_it_is_the_plea() {
        let root = temp();
        write(
            &root,
            "pleas.md",
            "## Nightly Regression\n\nRoutine and never urgent.\n\n## Production incident\n\nWorth the maximum.\n",
        );
        let n = Nivedanas::load(&root).unwrap();
        assert_eq!(n.aliases(), vec!["nightly-regression", "production-incident"]);
        assert_eq!(n.get("nightly-regression").unwrap().prose, "Routine and never urgent.");
        // The heading is kept as written, for showing people.
        assert_eq!(n.get("nightly-regression").unwrap().title, "Nightly Regression");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_plea_can_be_spelled_the_way_anyone_would_spell_it() {
        // The alias exists to be typed, by a person at a terminal and by a CI job that
        // interpolated a branch name. One that only matches its own exact punctuation
        // would be answered with a refusal most of the time.
        let root = temp();
        write(&root, "p.md", "# Nightly Regression\n\nRoutine.\n");
        let n = Nivedanas::load(&root).unwrap();
        for spelling in
            ["nightly-regression", "Nightly Regression", "nightly_regression", "NIGHTLY  regression"]
        {
            assert!(n.get(spelling).is_some(), "{spelling:?}");
        }
        assert!(n.get("nightly").is_none(), "a prefix is not a match");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn pleas_may_be_split_across_files() {
        let root = temp();
        write(&root, "a.md", "## alpha\n\nfirst.\n");
        write(&root, "b.md", "## beta\n\nsecond.\n");
        let n = Nivedanas::load(&root).unwrap();
        assert_eq!(n.aliases(), vec!["alpha", "beta"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn two_headings_that_mean_one_alias_are_refused() {
        // Resolving it either way would be a silent choice about whose plea a caller
        // gets when they name it.
        let root = temp();
        write(&root, "a.md", "## Big Run\n\none.\n\n## big-run\n\ntwo.\n");
        let e = Nivedanas::load(&root).unwrap_err().to_string();
        assert!(e.contains("same plea"), "{e}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_empty_heading_is_not_a_plea() {
        // It would tell the model nothing, so a caller naming it would get a decision
        // made on no information. Better that the alias simply does not exist.
        let root = temp();
        write(&root, "a.md", "## nothing here\n\n## real\n\nsomething.\n");
        let n = Nivedanas::load(&root).unwrap();
        assert_eq!(n.aliases(), vec!["real"]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_hash_inside_a_fence_is_not_a_heading() {
        let root = temp();
        write(&root, "a.md", "## deploy\n\nRun it:\n\n```sh\n# not a heading\nmake\n```\n");
        let n = Nivedanas::load(&root).unwrap();
        assert_eq!(n.aliases(), vec!["deploy"]);
        assert!(n.get("deploy").unwrap().prose.contains("# not a heading"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_catalogue_is_every_plea_in_a_stable_order() {
        // It goes in the cached half of the prompt, so a catalogue that reshuffled
        // itself between two pleas would throw the cache away for nothing.
        let root = temp();
        write(&root, "a.md", "## zeta\n\nlast.\n\n## alpha\n\nfirst.\n");
        let n = Nivedanas::load(&root).unwrap();
        let once = n.catalogue();
        assert_eq!(once, Nivedanas::load(&root).unwrap().catalogue());
        assert!(once.find("### alpha") < once.find("### zeta"), "{once}");
        std::fs::remove_dir_all(&root).ok();
    }
}
