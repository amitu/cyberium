//! What a tester and a controller say to each other.
//!
//! The vocabulary is deliberate rather than REST-shaped, because the roles are
//! asymmetric and ritual. A *nivedana* is a plea that may be granted, countered or
//! questioned — not a POST. Naming the grammar keeps the asymmetry visible at
//! every call site.
//!
//! This is cm's own protocol. sirji hands over an authenticated stream and says
//! who is on it; everything here is ours.

use serde::{Deserialize, Serialize};

/// The tester's opening plea.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Nivedana {
    /// Free-form English. The developer's actual reason, which is the input the
    /// policy is weighed against.
    pub why: String,
    /// How many machines they think they need.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// What kind. Left to the org's own vocabulary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// Where this is running from — a CI job, a laptop, a nightly. The policy
    /// author decides what these mean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// The controller's answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum Verdict {
    /// Granted, with the machines to use.
    Grant {
        workers: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rationale: Option<String>,
    },
    /// A smaller or different shape fits the policy.
    Counter {
        count: u32,
        rationale: String,
    },
    /// Refused, with a reason the requester can act on.
    Deny { rationale: String },
}
