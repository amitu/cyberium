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
    /// Ask what *would* happen, and take nothing.
    ///
    /// The same plea down the same path — policy, then the fleet's own selection —
    /// stopping short of holding anything. A separate variant rather than a flag on
    /// `Nivedana` because it is a different intent, not a different request: what
    /// gets weighed is identical, and only the controller's last step differs.
    Rehearse(Nivedana),
    /// Ask for nothing at all.
    ///
    /// Answered by any controller that admitted the caller, which makes it a
    /// positive test of the whole chain — resolution, ticket, signature, dial —
    /// without taking a machine away from anybody to prove it.
    Ping,
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
    /// What a rehearsal would have got, as things stand right now.
    ///
    /// No machines, no reservation, no handles — the handles would be a way to learn
    /// fleet addresses without holding anything, and useless anyway, since a worker
    /// refuses work for a reservation it was never told about.
    ///
    /// A snapshot, not a promise: by the time the caller asks for real, the fleet
    /// has moved.
    Would { count: u32, rationale: String },

    /// The answer to a ping: we are here, and we accepted your ticket.
    ///
    /// Deliberately empty. It used to carry a fleet summary, justified as
    /// disclosing nothing a grant would not — which was wrong. A grant tells you
    /// about *your* request; a summary polled every minute tells another
    /// organisation your utilisation over time, and from that your release cadence,
    /// your team's size, and how often you have incidents.
    ///
    /// What a caller actually needs is answered better by asking: the fleet's two
    /// shortfalls are scoped to their own request — nothing here can ever do this,
    /// versus everything that can is busy — and neither generalises into
    /// intelligence about the fleet.
    Pong,
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
        /// Run by a shell, so a caller can send what they would have typed.
        command: String,
        /// Get the code first, and run in what that produces.
        ///
        /// Mutually exclusive with `cwd`: either the worker fetches a workspace or
        /// it uses one already on the machine, and a caller who asks for both has
        /// not decided which.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<Workspace>,
        /// Where to run it, when the machine already has the code. The worker
        /// refuses a path it cannot enter rather than silently running elsewhere.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Extra environment for the command. This is how a shard number reaches a
        /// test runner that reads it from the environment.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        env: Vec<(String, String)>,
        /// Files to send back when it finishes, relative to `cwd`.
        ///
        /// Results have to travel as bytes, not as paths. A worker on another
        /// machine shares no filesystem with the caller, and a design that works
        /// only when it does is a design that has not left the laptop.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        collect: Vec<String>,
        /// Which shard of the plan this machine takes.
        index: u32,
        total: u32,
    },
}

/// What comes back up the same stream, in order: any number of `Log`s while the
/// command runs, then exactly one `Done` or `No`.
///
/// Live output matters for a test runner. Holding it all until the end would mean
/// a suite that takes ten minutes says nothing for ten minutes, and the first thing
/// anyone would build on top is a way to see it sooner.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum Outcome {
    Log {
        index: u32,
        line: String,
        /// Which stream it came from, since a runner's summary and its errors are
        /// worth telling apart.
        stderr: bool,
    },
    Done {
        worker: String,
        index: u32,
        /// The command's exit status. `None` means it was killed for exceeding its
        /// limit, which is not the same as failing.
        code: Option<i32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        artifacts: Vec<Artifact>,
    },
    No {
        reason: String,
    },
}

/// The code a machine needs before it can run anything.
///
/// A worker starts with nothing. It does not have your repo, and a fleet that
/// assumed it did would only ever work on machines somebody had already prepared by
/// hand — which is the same as having no fleet.
///
/// Fetched fresh per reservation into a directory of its own, and deleted when the
/// reservation ends. Nothing is reused between runs: a working tree left over from
/// somebody else's job is how a green suite starts depending on what ran before it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    /// Anything `git fetch` understands. It travels to the worker as written, so a
    /// URL with a token in it hands that token to whoever runs the machine.
    pub repo: String,
    /// A commit, branch or tag. A commit is the honest choice — a branch means two
    /// shards can disagree about what they tested.
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Subdirectory of the checkout to work in, for a suite that does not live at
    /// the repository root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    /// Run once after checkout, before the command: `npm ci`, `bundle install`.
    /// Its output is streamed like any other, because "why is this slow" is
    /// usually answered here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
}

/// A file coming back from a worker, base64 so it survives a JSON line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// The path asked for, as asked for, so the caller can match it up.
    pub path: String,
    pub base64: String,
}
