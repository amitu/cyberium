//! Receiving a policy folder from the machine that has it in git.
//!
//! Three things stand between an upload and a tenant's directory, and each of them is
//! here because the alternative fails quietly:
//!
//! 1. **Who.** Only an admin named in `tenant.toml`. The host owns that file, and has to:
//!    a tenant that could name its own admins could grant itself the right to rewrite its
//!    own rules, and "who may change this" would answer itself. This is the one question
//!    about a tenant that is not the policy's to decide.
//! 2. **Where.** Every path is validated before anything is written. It came from another
//!    machine, so only its shape is ours to trust — `../../etc/anything` is a path a
//!    caller may send and must never be a path we open.
//! 3. **What.** The folder has to parse and fit before it replaces the one that works. A
//!    policy accepted and then found unreadable takes the tenant down at the next plea,
//!    which is a long way from where the mistake was made.
//!
//! Then it replaces the lot. Not a merge: the folder *is* the policy, so a merge leaves
//! files behind that nobody remembers writing and no repository contains, and the
//! controller ends up enforcing a mixture that exists nowhere. Wholesale means what runs
//! is what somebody can read in git.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::proto::{PolicyFile, Upload};

/// Everything the controller keeps for itself, whatever an upload says.
///
/// `tenant.toml` is the host's terms — a tenant that could overwrite it could raise its
/// own ceiling — and the ledger is what they have already spent, which is not theirs to
/// edit either. Both are also invisible to them: neither goes into the prompt, so an
/// upload replacing the folder has no reason to contain either.
fn ours(name: &str) -> bool {
    name == crate::tenant::FILE || name == crate::budget::FILE
}

/// Check a path from another machine before it becomes a path we write to.
///
/// Subdirectories are allowed, because `nivedanas/support/escalation.md` is a real thing
/// somebody wants; everything that could leave the folder is not.
pub fn safe(path: &str) -> Result<PathBuf> {
    if path.is_empty() {
        bail!("a file with no name");
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        bail!("{path}: absolute paths are not accepted");
    }
    let mut out = PathBuf::new();
    for part in candidate.components() {
        match part {
            std::path::Component::Normal(name) => {
                let Some(text) = name.to_str() else { bail!("{path}: not text") };
                // A leading dot hides a file from the folder listing a person reads,
                // which is a poor place for a rule to live.
                if text.starts_with('.') {
                    bail!("{path}: names beginning with a dot are not accepted");
                }
                out.push(text);
            }
            // `..`, `/`, `C:` — every one of them a way out of the directory.
            other => bail!("{path}: {other:?} is not allowed in a policy path"),
        }
    }
    if out.components().count() > 4 {
        bail!("{path}: nested deeper than anybody will read");
    }
    if ours(&out.display().to_string()) {
        bail!("{path} belongs to the host, not the tenant");
    }
    Ok(out)
}

/// Write an upload into a tenant's directory, replacing what is there.
///
/// Validated first, then staged beside the real directory, then swapped. The reason for
/// the dance: a tenant is being served while this runs, and a half-written folder is a
/// policy that never existed — worse than the old one and worse than the new one.
pub fn accept(dir: &Path, upload: &Upload) -> Result<Vec<String>> {
    if upload.files.is_empty() {
        bail!("an upload with no files in it would leave nothing to weigh pleas against");
    }

    let mut checked: Vec<(PathBuf, &PolicyFile)> = Vec::new();
    let mut total = 0usize;
    for file in &upload.files {
        let path = safe(&file.path)?;
        total += file.text.len();
        checked.push((path, file));
    }
    // The same cap the reader enforces, applied here so an oversized folder is refused
    // where somebody can act on it rather than failing every plea afterwards.
    if total > crate::rulebook::MOST_BYTES {
        bail!(
            "{total} bytes, over the {} that can be weighed at once",
            crate::rulebook::MOST_BYTES
        );
    }

    let staging = dir.with_extension("incoming");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    for (path, file) in &checked {
        let target = staging.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, &file.text)
            .with_context(|| format!("writing {}", target.display()))?;
    }

    // Read it back the way a plea will. A policy whose fenced block does not parse would
    // refuse every request afterwards, and the person who could fix it would be looking
    // at a working repository.
    crate::policy::Policy::load(&staging).context("the uploaded policy could not be read")?;
    crate::rulebook::Rulebook::load(&staging).context("the uploaded folder could not be read")?;

    // Keep what is ours, drop what was theirs, move theirs in.
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if ours(name) {
            continue;
        }
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    for entry in std::fs::read_dir(&staging)? {
        let from = entry?.path();
        let Some(name) = from.file_name() else { continue };
        std::fs::rename(&from, dir.join(name))?;
    }
    let _ = std::fs::remove_dir_all(&staging);

    let mut written: Vec<String> =
        checked.iter().map(|(p, _)| p.display().to_string()).collect();
    written.sort();
    Ok(written)
}

/// Collect a folder to send. The same files the controller would read, and no others.
pub fn gather(dir: &Path) -> Result<Upload> {
    let mut files = Vec::new();
    walk(dir, dir, &mut files, 0)?;
    files.sort_by(|a: &PolicyFile, b: &PolicyFile| a.path.cmp(&b.path));
    if files.is_empty() {
        bail!("{} has no policy files in it", dir.display());
    }
    Ok(Upload { files })
}

fn walk(dir: &Path, base: &Path, out: &mut Vec<PolicyFile>, depth: usize) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if name.starts_with('.') || ours(name) {
            continue;
        }
        if path.is_dir() {
            // `policy-tests/` stays home. The controller does not run them, and they hold
            // the expected answers — see `policytest`.
            if name != crate::policytest::DIR && depth < 3 {
                walk(&path, base, out, depth + 1)?;
            }
            continue;
        }
        if !path
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| matches!(x, "md" | "txt" | "yaml" | "yml" | "toml" | "json"))
        {
            continue;
        }
        out.push(PolicyFile {
            // Forward slashes on the wire whatever this machine uses, so a policy
            // uploaded from Windows reads the same to the controller as one from a Mac.
            path: path
                .strip_prefix(base)
                .unwrap_or(&path)
                .components()
                .filter_map(|c| c.as_os_str().to_str())
                .collect::<Vec<_>>()
                .join("/"),
            text: std::fs::read_to_string(&path)?,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, text: &str) -> PolicyFile {
        PolicyFile { path: path.into(), text: text.into() }
    }

    #[test]
    fn a_path_that_could_leave_the_folder_is_refused() {
        // It arrived from another machine. Only its shape is ours to trust.
        for bad in [
            "../escaped.md",
            "nivedanas/../../escaped.md",
            "/etc/passwd",
            "./x/../../y.md",
            "",
        ] {
            assert!(safe(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn a_subdirectory_is_fine_because_people_use_them() {
        assert_eq!(safe("policy.md").unwrap(), PathBuf::from("policy.md"));
        assert_eq!(
            safe("nivedanas/support/escalation.md").unwrap(),
            PathBuf::from("nivedanas/support/escalation.md")
        );
    }

    #[test]
    fn the_hosts_own_files_cannot_be_uploaded_over() {
        // A tenant that could overwrite `tenant.toml` could raise its own ceiling, and one
        // that could overwrite the ledger could forget what it had spent.
        assert!(safe("tenant.toml").is_err());
        assert!(safe("spend.log").is_err());
    }

    #[test]
    fn a_hidden_file_is_refused() {
        // A rule nobody sees in a directory listing is a poor place for a rule.
        assert!(safe(".secret.md").is_err());
        assert!(safe("nivedanas/.hidden.md").is_err());
    }

    #[test]
    fn accepting_replaces_rather_than_merges() {
        // The folder is the policy, so a leftover file is a rule that exists on the
        // controller and in no repository.
        let dir = crate::testing::scratch("upload");
        std::fs::write(dir.join("tenant.toml"), "ceiling = 3\n").unwrap();
        std::fs::write(dir.join("spend.log"), "1 2\n").unwrap();
        std::fs::write(dir.join("policy.md"), "old").unwrap();
        std::fs::write(dir.join("forgotten.md"), "a rule nobody remembers").unwrap();

        let written = accept(
            &dir,
            &Upload { files: vec![file("policy.md", "new"), file("nivedanas/a.md", "## x\n\ny")] },
        )
        .unwrap();
        assert_eq!(written, vec!["nivedanas/a.md", "policy.md"]);
        assert_eq!(std::fs::read_to_string(dir.join("policy.md")).unwrap(), "new");
        assert!(!dir.join("forgotten.md").exists(), "the old file survived");
        assert!(dir.join("nivedanas/a.md").exists());
        // And the host's files are still the host's.
        assert!(dir.join("tenant.toml").exists());
        assert!(dir.join("spend.log").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unreadable_policy_is_refused_before_it_replaces_a_working_one() {
        // Accepted and then found broken means every plea afterwards fails, while the
        // person who could fix it is looking at a repository that seems fine.
        let dir = crate::testing::scratch("upload-bad");
        std::fs::write(dir.join("policy.md"), "the one that works").unwrap();

        let e = accept(
            &dir,
            &Upload { files: vec![file("policy.md", "```yaml\nstanding_limit: lots\n```")] },
        )
        .unwrap_err();
        assert!(format!("{e:#}").contains("could not be read"), "{e:#}");
        assert_eq!(
            std::fs::read_to_string(dir.join("policy.md")).unwrap(),
            "the one that works",
            "a refused upload left the tenant changed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_upload_is_refused() {
        // It would leave nothing to weigh pleas against, which is not a policy anybody
        // meant to write.
        let dir = crate::testing::scratch("upload-empty");
        assert!(accept(&dir, &Upload { files: vec![] }).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn gathering_leaves_the_tests_and_the_hosts_files_at_home() {
        let dir = crate::testing::scratch("gather");
        std::fs::write(dir.join("policy.md"), "mine").unwrap();
        std::fs::write(dir.join("tenant.toml"), "ceiling = 3").unwrap();
        std::fs::create_dir_all(dir.join(crate::policytest::DIR)).unwrap();
        std::fs::write(dir.join(crate::policytest::DIR).join("c.json"), "[]").unwrap();
        std::fs::create_dir_all(dir.join("nivedanas")).unwrap();
        std::fs::write(dir.join("nivedanas/x.md"), "## a\n\nb").unwrap();

        let paths: Vec<String> = gather(&dir).unwrap().files.into_iter().map(|f| f.path).collect();
        assert_eq!(paths, vec!["nivedanas/x.md", "policy.md"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
