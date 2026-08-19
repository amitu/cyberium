//! Everything a tenant has written down, as one block of context.
//!
//! A file tree, then every file's contents. No parsing, no schema, no structure imposed
//! by cm: whatever is in the folder is what the model reads, and one pass over it
//! produces the answer.
//!
//! This replaced three earlier attempts, each of which was cm deciding how organisations
//! are allowed to think:
//!
//! - `policy.md` split into a deterministic block and "the prose", with the prose being
//!   the only part a model saw.
//! - `nivedanas/`, whose markdown headings cm parsed into a catalogue of named pleas.
//! - Then a `group:` on each plea, taken from its subdirectory.
//!
//! Every one of those made a layout mean something. But a group is not a structure — it
//! is whatever an organisation finds itself saying, and the sentence it wants to write is
//! as likely to be "the pleas in `support-pleas.md`" as "the `noisy-users` folder" or
//! "`cut-a-release` and `smoke-the-candidate`". Each of those needs a *different* schema,
//! and a folder listing needs none of them:
//!
//! ```markdown
//! Amit experiments constantly, so he may only use pleas from the `noisy-users` folder.
//! The support team works from `support-pleas.md`. The release pleas — `cut-a-release`
//! and `smoke-the-candidate` — are for whoever is on release duty. Everybody else may
//! name any plea, or explain themselves in their own words if none fit.
//! ```
//!
//! Three groupings, three shapes, one paragraph, no fields.
//!
//! What is still parsed out of `policy.md` is the fenced block, and only because cm
//! *enforces* those: who may ask at all, the ceiling on any answer, how long a grant
//! lasts, what the budget is. They are also shown here in full, because a model asked to
//! reach the answer in one pass should be able to see the same numbers it is being held
//! to.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// A tenant's whole folder is re-read on every plea and sent every time, so it has to
/// stay the sort of thing a person maintains. Well past any real policy, and far enough
/// under a context window that the operational state and the plea have room.
const MOST_BYTES: usize = 256 * 1024;

/// Files that are not the tenant's own writing.
///
/// `tenant.toml` is the *host's* terms for them — a tenant reading their own ceiling as
/// though they had set it would be reading somebody else's rule as their own. The ledger
/// is operational state, and the numbers that matter from it are computed and passed
/// separately rather than asking a model to add up a log.
fn theirs_to_write(name: &str) -> bool {
    name != crate::tenant::FILE && name != crate::budget::FILE
}

#[derive(Debug, Clone, Default)]
pub struct Rulebook {
    rendered: String,
}

impl Rulebook {
    pub fn load(dir: &Path) -> Result<Self> {
        let mut files = Vec::new();
        collect(dir, &mut files, 0)?;
        files.sort();

        let total: usize = files.iter().filter_map(|p| p.metadata().ok()).map(|m| m.len() as usize).sum();
        if total > MOST_BYTES {
            // Refused rather than truncated. Half a policy enforced as though it were the
            // whole one is the worst outcome available here, and it would be invisible.
            bail!(
                "this tenant's folder is {total} bytes, over the {MOST_BYTES} that can be \
                 weighed at once. It is read in full on every request, so it has to stay \
                 something a person maintains."
            );
        }

        let mut tree = String::new();
        let mut bodies = String::new();
        for path in &files {
            let name = path.strip_prefix(dir).unwrap_or(path).display().to_string();
            tree.push_str(&format!("  {name}\n"));
            let text = std::fs::read_to_string(path)?;
            // Fenced by path so a model can tell one file from the next, and so a rule
            // about "the file called x" has something to match on.
            bodies.push_str(&format!("--- {name} ---\n{}\n\n", text.trim_end()));
        }

        let rendered = if files.is_empty() {
            String::new()
        } else {
            format!("FILES\n{tree}\nCONTENTS\n{bodies}")
        };
        Ok(Self { rendered })
    }

    pub fn as_str(&self) -> &str {
        &self.rendered
    }
}

/// Every text file, whatever the layout.
///
/// An earlier version stopped after one level, which silently dropped
/// `nivedanas/noisy-users/big.md` — measured from the tenant's root, the useful nesting
/// starts at *two*. Rather than move the line, there is no line worth defending: the tree
/// is shown to the model, so a layout describes itself, and picking a depth would be one
/// more way of legislating how a folder may be arranged. The cap is only a stop against
/// something pathological.
const DEEPEST: usize = 8;
fn collect(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if depth < DEEPEST {
                collect(&path, out, depth + 1)?;
            }
            continue;
        }
        // Extension-based, because a model reads text and a `.zip` in a policy folder is
        // somebody's mistake rather than a rule.
        let readable = path
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| matches!(x, "md" | "txt" | "yaml" | "yml" | "toml" | "json"));
        if readable && theirs_to_write(name) {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        crate::testing::scratch("rulebook")
    }

    #[test]
    fn the_tree_and_every_file_arrive_together() {
        let root = temp();
        std::fs::write(root.join("policy.md"), "Be reasonable.").unwrap();
        std::fs::create_dir_all(root.join("nivedanas")).unwrap();
        std::fs::write(root.join("nivedanas/routine.md"), "## nightly\n\nRoutine.").unwrap();

        let book = Rulebook::load(&root).unwrap();
        let text = book.as_str();
        // The tree, so a rule can name a file or a folder it does not quote.
        assert!(text.contains("policy.md"), "{text}");
        assert!(text.contains("nivedanas/routine.md"), "{text}");
        // And the contents, so it can be weighed.
        assert!(text.contains("Be reasonable."), "{text}");
        assert!(text.contains("Routine."), "{text}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_folder_inside_a_folder_is_not_silently_dropped() {
        // `nivedanas/noisy-users/big.md` is two levels from the tenant root, and an
        // earlier one-level walk skipped it — so a policy naming that folder was talking
        // about pleas the model had never been shown.
        let root = temp();
        std::fs::create_dir_all(root.join("nivedanas/noisy-users")).unwrap();
        std::fs::write(root.join("nivedanas/noisy-users/big.md"), "## bisect\n\nSlow.").unwrap();
        let text = Rulebook::load(&root).unwrap().as_str().to_string();
        assert!(text.contains("nivedanas/noisy-users/big.md"), "{text}");
        assert!(text.contains("Slow."), "{text}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_layout_is_reported_and_not_interpreted() {
        // The whole point: cm never decides that a folder means a group, or that a
        // heading is an alias. It says where things are, and the policy says what that
        // implies — which is the only way one paragraph can group by folder, by file and
        // by name at once.
        let root = temp();
        std::fs::create_dir_all(root.join("noisy-users")).unwrap();
        std::fs::write(root.join("noisy-users/big.md"), "## bisect everything\n\nSlow.").unwrap();
        let text = Rulebook::load(&root).unwrap().as_str().to_string();
        assert!(text.contains("noisy-users/big.md"), "the path is a fact: {text}");
        assert!(!text.contains("group"), "cm adds no vocabulary of its own: {text}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_hosts_own_terms_are_not_shown_as_the_tenants_writing() {
        // `tenant.toml` is what the host imposes. A tenant reading their ceiling as
        // though they had chosen it would be reading somebody else's rule as their own.
        let root = temp();
        std::fs::write(root.join("tenant.toml"), "ceiling = 3\n").unwrap();
        std::fs::write(root.join("spend.log"), "1755000000 5\n").unwrap();
        std::fs::write(root.join("policy.md"), "Mine.").unwrap();
        let text = Rulebook::load(&root).unwrap().as_str().to_string();
        assert!(text.contains("Mine."));
        assert!(!text.contains("ceiling = 3"), "{text}");
        assert!(!text.contains("1755000000"), "the ledger is state, not a rule");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn an_empty_folder_renders_to_nothing_rather_than_an_empty_heading() {
        // A "FILES" heading with nothing under it invites a model to wonder what it was
        // meant to see there.
        let root = temp();
        assert!(Rulebook::load(&root).unwrap().as_str().is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_same_folder_renders_identically_every_time() {
        // It is the cached half of the prompt. A rendering that reordered itself between
        // two requests would throw the cache away for nothing.
        let root = temp();
        std::fs::write(root.join("b.md"), "second").unwrap();
        std::fs::write(root.join("a.md"), "first").unwrap();
        let once = Rulebook::load(&root).unwrap().as_str().to_string();
        assert_eq!(once, Rulebook::load(&root).unwrap().as_str());
        assert!(once.find("a.md") < once.find("b.md"), "sorted: {once}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_folder_too_big_to_weigh_is_an_error_not_a_truncation() {
        let root = temp();
        std::fs::write(root.join("huge.md"), "x".repeat(MOST_BYTES + 1)).unwrap();
        let e = Rulebook::load(&root).unwrap_err().to_string();
        assert!(e.contains("over the"), "{e}");
        std::fs::remove_dir_all(&root).ok();
    }
}
