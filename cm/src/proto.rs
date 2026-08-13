//! What testers, controllers and workers say to each other.
//!
//! The vocabulary is deliberate rather than REST-shaped, because the roles are
//! asymmetric and ritual. A *nivedana* is a plea that may be granted, countered or
//! refused — not a POST. An *aadesh* is an order with limits attached, not a PUT.
//! Naming the grammar keeps the asymmetry visible at every call site.
//!
//! Three parties, and only two conversations:
//!
//! ```text
//!   worker  ──register──▶  controller      the connection is the registration
//!   tester  ──nivedana──▶  controller  ──aadesh──▶  worker
//!   tester  ──upadesh───▶  worker           direct; the controller is not a proxy
//! ```
//!
//! **Workers never talk to each other.** They have nothing to say: allocation,
//! availability and timeouts all live at the controller, which is the only party
//! that can see the whole fleet.
//!
//! This is cm's own protocol. sirji hands over an authenticated stream and says
//! who is on it; everything here is ours.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// tester -> controller
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "kebab-case")]
pub enum Plea {
    /// Ask for machines.
    Nivedana(Nivedana),
    /// Done with them. Sent the moment the work finishes: a duration hint sizes a
    /// plan, it never justifies holding capacity idle.
    Release { reservation: String },
}

/// The plea.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Nivedana {
    /// Free-form English — the developer's actual reason, and the input the
    /// policy's prose is weighed against.
    pub why: String,
    /// How many machines they think they need.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// What the work needs a machine to be able to do. Matched against what each
    /// worker declared; a worker missing any of these is not a candidate.
    ///
    /// Plain strings on purpose. The org invents its own vocabulary — `linux`,
    /// `gpu`, `ios-17`, `has-2fa-sim` — and nothing here needs to understand any
    /// of it to match on it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Where this is running from: a laptop, a CI job, a nightly. The policy
    /// author decides what these mean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// The controller's answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "kebab-case")]
pub enum Verdict {
    /// Granted: the reservation, and how to reach each machine.
    Grant {
        reservation: String,
        workers: Vec<WorkerHandle>,
        /// Seconds until the reservation expires on its own. Releasing early is
        /// expected; this is the backstop for a caller that dies mid-run and would
        /// otherwise strand capacity forever.
        expires_in: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rationale: Option<String>,
    },
    /// A different shape fits: fewer machines than asked for, and why.
    Counter { count: u32, rationale: String },
    /// Refused, with a reason the caller can act on.
    Deny { rationale: String },
    /// Acknowledged — the answer to a release.
    Ok,
}

/// Where a granted machine is, and the ticket that admits us to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerHandle {
    pub name: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
    /// Signed by the controller and bound to our key. The worker verifies it and
    /// thereby learns it has been assigned to us — it holds no roster of its own.
    pub ticket: sirji::Ticket,
}

// ---------------------------------------------------------------------------
// worker <-> controller
// ---------------------------------------------------------------------------

/// A worker arriving. The connection *is* the registration: while it is open the
/// worker is available, and when it drops the controller stops offering it. No
/// heartbeat, because QUIC already reports a peer going away.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Register {
    /// What the worker calls itself, so a caller can be told something meaningful.
    pub name: String,
    /// How many jobs it will take at once.
    pub slots: u32,
    /// What it can do. Matched against what a plea asks for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Where it listens, so a caller can be sent straight to it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

/// Sent down the registration connection when the worker is assigned.
///
/// A worker never re-evaluates anything against text: it is told, in structured
/// fields, and obeys. Nothing at the edge needs a model or a policy file, which is
/// what keeps a worker cheap enough to run hundreds of.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "order", rename_all = "kebab-case")]
pub enum Aadesh {
    /// You are assigned to `caller` for this reservation, within these limits.
    Assigned {
        reservation: String,
        caller: String,
        limits: Limits,
    },
    /// That reservation is over — released, or expired. Stop accepting from it.
    Freed { reservation: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Limits {
    pub max_seconds: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_seconds: 600 }
    }
}

// ---------------------------------------------------------------------------
// tester -> worker
// ---------------------------------------------------------------------------

/// The work itself, sent straight to the worker. The controller allocated; it does
/// not carry the traffic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Upadesh {
    Run {
        reservation: String,
        command: String,
        /// Which shard of the plan this machine takes.
        index: u32,
        total: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum Outcome {
    Done {
        worker: String,
        index: u32,
        output: String,
    },
    No { reason: String },
}
