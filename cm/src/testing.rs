//! Scratch directories for tests, and one guarantee: no two of them are the same.
//!
//! Public rather than `#[cfg(test)]`, because anybody writing a controller against this
//! crate needs the same thing, and the trap below is not obvious enough to leave them to
//! rediscover.
//!
//! The obvious construction — process id plus a nanosecond timestamp — is not unique.
//! Every test in a binary shares the pid, tests run in parallel, and the clock's real
//! granularity is coarser than a nanosecond, so two directories collide often enough
//! to matter: one test then loads the other's files and fails on a result nothing in
//! its own body explains.
//!
//! A counter cannot do that.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A fresh directory that exists, named after what is being tested.
///
/// The pid is still in the name so a leftover from one run cannot be read by the next;
/// the counter is what keeps two tests in *this* run apart.
pub fn scratch(prefix: &str) -> PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("cm-{prefix}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("creating a scratch directory");
    dir
}
