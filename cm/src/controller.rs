//! The controller: everything that has a view of the whole fleet.
//!
//! Public because a deployment with its own identity service builds its own binary
//! around this rather than reimplementing the protocol.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;

use crate::fleet::{Fleet, Shortfall, Worker};
use crate::proto::{Aadesh, Answer, Knock, Nivedana, Plea, Register, Verdict, WorkerHandle};
use crate::directory::{self, Directory};
use crate::{admin, adviser, attest, budget, enrolled, fleet, proto, tenant};
use crate::{Config, home, keys, listening, load_config, quiet, read_line, write_line};
use sirji::id52;

// ---------------------------------------------------------------------------
// the controller
// ---------------------------------------------------------------------------

/// Everything the controller owns.
///
/// One lock over the fleet, because allocation has to be atomic across the roster
/// and the reservations together — two locks here is how a machine gets promised
/// to two callers.
pub struct Control {
    config: Config,
    /// One policy per tenant, keyed by the alias our own sirji minted into their
    /// ticket. Behind a lock because it re-reads from disk, so a policy edit or a
    /// new tenant takes effect without a restart.
    /// What this controller knows about the people asking. A trait, because `cm`'s
    /// answer — folders and files — is the right one for an organisation with no identity
    /// service and the wrong one for a company that has groups, sub-groups and feature
    /// flags in a system of its own.
    pub directory: Box<dyn directory::Directory>,
    /// Whose word this controller accepts besides its own sirji's. Read at startup and
    /// not re-read: this decides who counts as proven, and a list that reloads itself is
    /// a list a stray file write can extend.
    pub issuers: attest::Issuers,
    /// Keys this controller has agreed to remember. Behind a lock because it is written
    /// while running — unlike the two lists above, which say who is *trusted* and are read
    /// once, this is a consequence of trust already granted.
    pub enrolled: Mutex<enrolled::Keys>,
    /// Who may look at and change how this controller runs. Read at startup and not
    /// re-read: adding an admin is rare, deliberate, and worth a restart, and a list
    /// that reloads itself is a list a stray file write can extend.
    admins: admin::Admins,
    /// Weighs the prose half of a tenant's policy. `Unwired` when no model is
    /// configured, which is a working controller rather than a broken one.
    adviser: adviser::Claude,
    fleet: Mutex<Fleet>,
    /// A channel per registered worker, for pushing orders down its open stream.
    orders: Mutex<std::collections::BTreeMap<String, tokio::sync::mpsc::Sender<Aadesh>>>,
    signing: sirji::SecretKey,
}

/// The reference controller: tenants in folders under the configured root.
///
/// Every tenant is read here, not lazily: configuration that does not parse should stop a
/// controller starting rather than surface an hour later on somebody's first plea, with
/// them waiting on the other end.
pub async fn controller() -> Result<()> {
    let config = load_config(&home()?)?;
    run(Box::new(directory::Folders::load(&config.root)?)).await
}

/// Run a controller with your own directory behind it.
///
/// The entry point for a deployment whose callers live somewhere other than a folder: a
/// company with groups, sub-groups, user ids and feature flags implements
/// [`directory::Directory`] against its own systems and calls this. Everything else — the
/// protocol, the fleet, the model call, the decision — is shared, and `cm test` and
/// `cm worker` do not know the difference.
pub async fn run(directory: Box<dyn Directory>) -> Result<()> {
    let home = home()?;
    sirji::Settings::load(&home)?.activate();
    let config = load_config(&home)?;

    println!("controller `{}` listening as {}", config.name, config.key);
    println!("callers known from: {}", directory.describe());
    match directory.roster().await {
        // Not an error — a controller with no tenants is correctly configured for
        // nobody. Said loudly because every plea will be refused until it is fixed, and
        // the refusal alone would not explain why.
        Ok(listed) if listed.is_empty() => {
            println!("no tenants — every plea will be refused.");
            println!("  add one with `cm tenant add <alias>`");
        }
        Ok(listed) => {
            let names: Vec<&str> = listed.iter().map(|t| t.tenant.as_str()).collect();
            println!("{} tenant(s): {}", names.len(), names.join(", "));
        }
        Err(e) => println!("cannot list tenants yet: {e:#}"),
    }

    // Read at startup: both of these decide who is believed, and neither should be
    // extendable by a file appearing on disk while the controller runs.
    let issuers = attest::Issuers::load(&config.root)?;
    println!("attestations accepted from: {}", issuers.describe());

    let remembered = enrolled::Keys::load(&config.root)?;
    if !remembered.is_empty() {
        println!("{} enrolled key(s)", remembered.len());
    }

    let admins = admin::Admins::load(&config.root)?;
    if admins.list.is_empty() {
        println!(
            "no admins in {} — nobody can look inside.\n  add one with `cm admin add <name> <id52>`",
            admin::Admins::path_in(&config.root).display()
        );
    } else {
        println!(
            "{} admin(s): {}",
            admins.list.len(),
            admins.list.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
        );
    }

    // Fatal, and fatal here rather than on the first caller's request. A controller
    // that came up without a model is one that cannot read anybody's policy, and the
    // place to learn that is a deploy log, not somebody's CI output.
    let adviser = adviser::Claude::from_env()?;
    println!("policy weighed by: {}", adviser.describe());

    let secret = keys(&home).secret(&id52::decode(&config.key)?)?;
    let endpoint = sirji::bind(secret.clone()).await?;

    let control = Arc::new(Control {
        fleet: Mutex::new(Fleet::default()),
        config: config.clone(),
        directory,
        issuers,
        enrolled: Mutex::new(remembered),
        admins,
        adviser,
        orders: Mutex::new(Default::default()),
        // Tickets admitting a caller to a worker are signed by us, and verified by
        // the worker against the controller key it registered with. The mechanism
        // sirji uses one level up, reused one level down.
        signing: secret,
    });

    let hints = listening(&endpoint).await;
    // Printed, because a caller proving itself by attestation has no parent to ask where
    // this is. It dials the key directly, so somebody has to be able to read the address
    // off a log and put it in a CI variable.
    if !hints.is_empty() {
        println!("reachable at: {}", hints.join(", "));
    }
    // The document a caller with no parent looks for. Printed rather than served, because
    // a controller serving HTTPS would need a certificate, a port and a name — and an
    // operator who has those already has somewhere to put a static file.
    if let Ok(doc) = serde_json::to_string(&crate::Published {
        key: config.key.clone(),
        hints: hints.clone(),
    }) {
        println!("publish at {}: {doc}", crate::WELL_KNOWN);
    }
    tokio::spawn({
        let config = config.clone();
        let home = home.clone();
        async move { register_with_parent(config, home, hints).await }
    });
    tokio::spawn(reap(control.clone()));

    while let Some(incoming) = endpoint.accept().await {
        let control = control.clone();
        tokio::spawn(async move {
            if let Err(e) = serve(incoming, control).await
                && !quiet(&e)
            {
                eprintln!("connection failed: {e:#}");
            }
        });
    }
    Ok(())
}

/// Hold a connection to our own sirji, so peers can resolve us.
async fn register_with_parent(
    config: Config,
    home: std::path::PathBuf,
    hints: Vec<String>,
) -> Result<()> {
    let store = keys(&home);
    let key = id52::decode(&config.key)?;
    loop {
        let secret = store.secret(&key)?;
        match sirji::daemon::register_device(&secret, &config.parent, &config.parent_hints, &hints)
            .await
        {
            Ok(()) => println!("parent connection closed; reconnecting"),
            Err(e) => eprintln!("cannot reach parent: {e:#}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

/// Take back reservations nobody released. The backstop for a caller that died.
async fn reap(control: Arc<Control>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let expired = control.fleet.lock().await.expire();
        for gone in expired {
            println!(
                "{}'s {} expired unreleased, taking back {} machine(s) — {} credit(s)",
                gone.alias(),
                gone.id(),
                gone.workers().len(),
                gone.credits
            );
            // Billed exactly like a release. A caller who walked away pays for the
            // time they held, which is what they cost the fleet.
            control.charge(&gone);
            control.tell_freed(gone.id(), gone.workers()).await;
        }
    }
}

impl Control {
    /// How many machines this caller is entitled to, by their tenant's policy and
    /// then their tenant's ceiling. `Err` carries the refusal to send back.
    ///
    /// Two clamps in order, and the order matters: the tenant's own policy divides
    /// what they have, and the host's ceiling decides how much that is. Without the
    /// second, an organisation writing its own `policy.md` would be writing its own
    /// quota.
    async fn entitlement(
        &self,
        alias: &str,
        // What an issuer vouched for about this caller, if one did. Merged with what the
        // deployment attests, because from a policy's side they are the same kind of
        // thing: facts nobody asking could have made up.
        vouched: &BTreeMap<String, String>,
        nivedana: &Nivedana,
    ) -> Result<Entitled> {
        // Everything the decision needs, gathered before anything slow happens — and
        // every lock released before the model call. A 30s network round trip under
        // the tenants or fleet mutex would serialise every plea in the fleet behind
        // one caller's request.
        let who = self
            .directory
            .look_up(alias)
            .await
            .with_context(|| format!("looking up {alias}"))?;
        let Some(who) = who else {
            // Named, but not onboarded. Distinguished from a refusal because the fix is
            // completely different — somebody has to run `cm tenant add`.
            return refused(format!("{alias} is not a tenant of this controller"));
        };
        if !who.may_ask {
            return refused(format!("{alias} may not ask this controller for machines"));
        }

        let tenant_name = who.tenant.clone();
        let lifetime = who.lifetime;
        let ceiling = who.ceiling;
        let wanted = nivedana.count.unwrap_or(1);
        let rulebook = who.rulebook.clone();

        // An empty instructions block is the one input that makes a model invent a rule
        // rather than apply one, so it is refused rather than sent. Reachable only if
        // somebody emptied the folder — `cm tenant add` writes a starter policy.
        if rulebook.trim().is_empty() {
            return refused(format!(
                "tenant `{tenant_name}` has nothing written down — no policy to weigh \
                 this against"
            ));
        }

        let spent = match who.budget {
            Some(b) => Some((b, self.directory.spent(&tenant_name, b.window).await?)),
            None => None,
        };
        let (brief, money) = {
            let fleet = self.fleet.lock().await;
            (
                fleet.brief(&nivedana.capabilities),
                spent.map(|(b, spent)| adviser::Money {
                    budget: b.credits,
                    spent,
                    committed: fleet.committed(&tenant_name),
                    window_secs: b.window,
                }),
            )
        };

        // Two answers that no policy can change, so no model is asked for them. Both
        // are certain, and they are told apart on purpose: one means give up, the other
        // means try again in a minute, and a caller with retry logic needs to know
        // which. (An earlier ordering ran the budget check first and reported *both*
        // of these as "budget spent", which sent people to look at their credits over
        // hardware they did not have.)
        if brief.capable == 0 {
            return refused(format!(
                "no machine in the fleet can do {:?} — waiting will not change that",
                nivedana.capabilities
            ));
        }
        if brief.free == 0 {
            return refused(format!(
                "{} machine(s) here can do {:?}, and none are free right now — \
                 this one is worth retrying",
                brief.capable, nivedana.capabilities
            ));
        }

        let advice = adviser::Advice {
            rulebook,
            attested: adviser::Attested {
                tenant: tenant_name.clone(),
                caller: alias.to_string(),
                facts: {
                    let mut facts = who.facts.clone();
                    // The deployment's own word wins a collision: it knows this caller,
                    // and an issuer's claim about a `plan` is a claim about a repository.
                    for (k, v) in vouched {
                        facts.entry(k.clone()).or_insert_with(|| v.clone());
                    }
                    facts
                },
            },
            declared: adviser::Declared {
                said: nivedana.said.clone(),
                count: wanted,
                capabilities: nivedana.capabilities.clone(),
            },
            standing: who.standing,
            ceiling,
            lifetime,
            fleet: brief.clone(),
            money: money.clone(),
        };

        // No fallback, on purpose, and no verdict either — this leaves as an error.
        // There is no number to substitute that any organisation asked for, and
        // inventing one would keep the fleet running while nobody's policy is applied.
        // A `?` is the whole safeguard: nothing downstream can mistake a fault for a
        // decision, because it never becomes one.
        let limit = Limit {
            ceiling,
            free: brief.free,
            // Every cap that applies is already folded into `ceiling` by the directory,
            // which is the only place that knows them all.
            host: ceiling,
            affordable: money.as_ref().map(|m| {
                budget::Room { budget: m.budget, spent: m.spent, committed: m.committed }
                    .affordable(&brief.rates, lifetime)
            }),
        };

        // The decision itself, shared verbatim with `cm policy-test`. Splitting it out is
        // the same rule as sharing `choose` with the dry run: a second implementation
        // would eventually disagree with this one, and be believed — a policy test that
        // passes against a slightly different decision than the fleet makes is worse than
        // no policy test at all.
        let weighed = weigh(
            &self.adviser,
            &advice,
            limit,
            Some(Review {
                directory: self.directory.as_ref(),
                about: directory::Weighing {
                    caller: alias,
                    tenancy: &who,
                    asked: wanted,
                    said: &nivedana.said,
                },
            }),
        )
        .await
        .with_context(|| format!("weighing {alias}'s plea"))?;

        for line in weighed.log(alias) {
            println!("{line}");
        }
        if let Some(fault) = weighed.fault_report(&tenant_name) {
            eprintln!("{fault}");
        }
        let Some((allowed, rationale)) = weighed.granted() else {
            return refused(weighed.refusal(wanted));
        };

        Ok(Ok((allowed, Some(rationale))))
    }

    /// Write a closed reservation into its tenant's ledger.
    ///
    /// Never fails the caller. The machines have already come back, and refusing to
    /// acknowledge that would strand them — an unrecorded line is a billing problem,
    /// a stuck reservation is an outage.
    fn charge(&self, closed: &fleet::Closed) {
        let dir = tenant::Tenants::dir_in(&self.config.root).join(closed.tenant());
        let entry = budget::Entry {
            at: fleet::now(),
            credits: closed.credits,
            reservation: closed.id().to_string(),
            minutes: closed.minutes,
            machines: closed.workers().to_vec(),
        };
        if let Err(e) = budget::record(&dir, &entry) {
            eprintln!("could not record {} credits for {}: {e:#}", closed.credits, closed.tenant());
        }
    }


    /// Remember the key this arrived on.
    ///
    /// Only from a connection that proved itself with an attestation, and only from an
    /// issuer the host marked `enrol = true`. Both matter: the first means the key being
    /// remembered is the key the token named, so nobody can enrol somebody else's; the
    /// second means a build token cannot leave a permanent key behind.
    async fn enrol(
        &self,
        alias: &str,
        key: &str,
        knock: &Knock,
        note: Option<&str>,
    ) -> Verdict {
        // Already remembered, or arrived on a ticket: either way there is no token here to
        // check, and enrolling on the strength of a connection alone would let anything
        // that could dial us stay forever.
        let Some(token) = &knock.attestation else {
            return Verdict::Deny {
                rationale: "enrolling needs an attestation on this same connection, so the \
                            key remembered is the key a token named"
                    .into(),
            };
        };
        let vouched = match self.issuers.verify(token, key).await {
            Ok(vouched) => vouched,
            Err(e) => return Verdict::Deny { rationale: format!("{e:#}") },
        };
        if !vouched.may_enrol {
            return Verdict::Deny {
                rationale: format!(
                    "tokens from this issuer prove who is asking but may not enrol a key — \
                     it proves a job rather than a machine. The host decides, with \
                     `enrol = true` in {}",
                    attest::FILE
                ),
            };
        }

        let entry = enrolled::Enrolled {
            key: key.to_string(),
            alias: vouched.alias.clone(),
            issuer: vouched.facts.get("issuer").cloned().unwrap_or_default(),
            at: fleet::now(),
            note: note.map(str::to_string),
        };
        match self.enrolled.lock().await.remember(entry) {
            Ok(()) => {
                println!("enrolled {key} as {} (asked by {alias})", vouched.alias);
                Verdict::Ok
            }
            Err(e) => {
                eprintln!("could not record an enrolment for {}: {e:#}", vouched.alias);
                Verdict::Deny { rationale: format!("could not record it: {e}") }
            }
        }
    }

    /// Stop remembering a key.
    ///
    /// Yours by default, or another of your *own* aliases' keys — one person with three
    /// laptops should be able to revoke the one on the train from either of the others,
    /// and should not be able to revoke anybody else's.
    async fn forget(&self, alias: &str, mine: &str, which: Option<&str>) -> Verdict {
        let target = which.unwrap_or(mine);
        let mut held = self.enrolled.lock().await;
        match held.who(target).map(|e| e.alias.clone()) {
            Some(owner) if owner != alias => Verdict::Deny {
                rationale: "that key is not yours to forget".into(),
            },
            None => Verdict::Deny {
                rationale: format!("this controller does not remember {target}"),
            },
            Some(_) => match held.forget(target) {
                Ok(Some(gone)) => {
                    println!("forgot {} ({})", gone.key, gone.alias);
                    Verdict::Ok
                }
                Ok(None) => Verdict::Deny { rationale: "nothing to forget".into() },
                Err(e) => Verdict::Deny { rationale: format!("could not record it: {e}") },
            },
        }
    }

    /// Take a policy folder from one of a tenant's admins.
    ///
    /// A verdict rather than an error: every outcome here is a decision about the
    /// caller's authority or their files, and nothing about it is undecidable.
    async fn accept_policy(&self, alias: &str, up: &proto::Upload) -> Verdict {
        let who = match self.directory.look_up(alias).await {
            Ok(Some(who)) => who,
            Ok(None) => {
                return Verdict::Deny {
                    rationale: format!("{alias} is not a tenant of this controller"),
                };
            }
            Err(e) => {
                eprintln!("could not look {alias} up: {e:#}");
                return Verdict::Deny { rationale: format!("could not look you up: {e}") };
            }
        };
        if !who.may_write {
            // Named, not authorised. Said as two different things because the fix is
            // different: one needs onboarding, the other needs somebody with the right.
            return Verdict::Deny {
                rationale: format!(
                    "{alias} may run tests for `{}` but not change its rules — whoever \
                     runs this controller decides that, not the rules themselves",
                    who.tenant
                ),
            };
        }

        match self.directory.write_rules(&who.tenant, up).await {
            Err(e) => {
                eprintln!("refused a policy for {} from {alias}: {e:#}", who.tenant);
                Verdict::Deny { rationale: format!("{e:#}") }
            }
            Ok(written) => {
                println!(
                    "{alias} replaced {}'s policy: {} file(s) — {}",
                    who.tenant,
                    written.len(),
                    written.join(", ")
                );
                Verdict::Ok
            }
        }
    }

    /// Which tenant this caller bills to. Not the caller: several share a tenant.
    async fn tenant_of(&self, alias: &str) -> String {
        match self.directory.look_up(alias).await {
            Ok(Some(who)) => who.tenant,
            _ => alias.to_string(),
        }
    }

    /// How long this tenant's grants last unreleased.
    async fn lifetime_for(&self, alias: &str) -> u64 {
        match self.directory.look_up(alias).await {
            Ok(Some(who)) => who.lifetime,
            _ => fleet::RESERVATION_SECS,
        }
    }

    /// What an operator asked to see.
    ///
    /// Rendered here rather than shipped as structures: this is the one view where
    /// holder aliases and machine names appear together, and keeping it inside the
    /// controller makes it obvious that none of it is on a path to a caller.
    async fn look(&self, what: proto::Look) -> proto::Sight {
        let fleet = self.fleet.lock().await;
        let mut lines = Vec::new();

        match what {
            proto::Look::Fleet => {
                let (machines, free, capabilities) = fleet.summary();
                lines.push(format!(
                    "{machines} machine(s), {free} free, can {capabilities:?}"
                ));
                for w in fleet.workers() {
                    let held = match &w.held_by {
                        None => "idle".to_string(),
                        Some(id) => format!("held by {id}"),
                    };
                    lines.push(format!(
                        "  {:<12} {:>3} credit(s)/min  can {:?}  {held}",
                        w.name, w.rate, w.capabilities
                    ));
                }
            }
            proto::Look::Spend => {
                // Every tenant at once, which only an admin ever sees — a tenant
                // learning its neighbours' spend would be the ping's fleet summary
                // all over again.
                drop(fleet);
                let listed = match self.directory.roster().await {
                    Ok(listed) => listed,
                    Err(e) => {
                        lines.push(format!("cannot read tenants: {e:#}"));
                        return proto::Sight { lines };
                    }
                };
                if listed.is_empty() {
                    lines.push("no tenants".into());
                }
                // Trouble first, because it changes how to read everything under it: a
                // tenant listed here is running on terms that are not the ones on disk.
                for t in listed.iter().filter(|t| t.unread.is_some()) {
                    lines.push(format!(
                        "  !! {}: configuration not read, still running on the last good \
                         copy — {}",
                        t.tenant,
                        t.unread.as_deref().unwrap_or("")
                    ));
                }
                for t in &listed {
                    let Some(b) = t.budget else {
                        // No budget is a real state, and "0 of 0" would read as a tenant
                        // that may spend nothing.
                        lines.push(format!("  {:<16} no budget set", t.tenant));
                        continue;
                    };
                    let spent = self.directory.spent(&t.tenant, b.window).await.unwrap_or(0);
                    let committed = self.fleet.lock().await.committed(&t.tenant);
                    let room = budget::Room { budget: b.credits, spent, committed };
                    lines.push(format!(
                        "  {:<16} {} of {} credit(s) used, {} committed, {} left",
                        t.tenant, room.spent, room.budget, room.committed, room.left()
                    ));
                }
                return proto::Sight { lines };
            }
            proto::Look::Reservations => {
                let now = fleet::now();
                let mut any = false;
                for r in fleet.reservations() {
                    any = true;
                    lines.push(format!(
                        "  {:<6} {:<12} {} machine(s), {}s left",
                        r.id,
                        r.alias,
                        r.workers.len(),
                        r.expires_at.saturating_sub(now)
                    ));
                }
                if !any {
                    lines.push("nothing is held".into());
                }
            }
        }
        proto::Sight { lines }
    }

    /// Tell each machine a reservation is over. Best-effort: one that has gone away
    /// needs no telling, and one that returns registers afresh.
    async fn tell_freed(&self, reservation: &str, workers: &[String]) {
        let orders = self.orders.lock().await;
        for name in workers {
            if let Some(tx) = orders.get(name) {
                let _ = tx
                    .send(Aadesh::Freed {
                        reservation: reservation.to_string(),
                    })
                    .await;
            }
        }
    }
}

pub async fn serve(incoming: sirji::Incoming, control: Arc<Control>) -> Result<()> {
    let conn = incoming.await?;
    let caller_key = conn.remote_id();
    let caller = id52::encode(&caller_key);

    let (mut send, recv) = conn.accept_bi().await?;
    let mut recv = BufReader::new(recv);

    // The first line says which conversation this is: a worker arriving carries a
    // Register, a tester carries a ticket.
    let mut line = String::new();
    recv.read_line(&mut line).await?;

    if let Ok(register) = serde_json::from_str::<Register>(line.trim()) {
        return serve_worker(control, conn, send, register, caller).await;
    }

    let knock: Knock = serde_json::from_str(line.trim())
        .map_err(|e| anyhow::anyhow!("unreadable opening line: {e}"))?;

    let who = match admit(&control, &knock, &caller_key).await {
        Ok(who) => who,
        Err(rationale) => {
            write_line(&mut send, &Answer::Decided(Verdict::Deny { rationale })).await?;
            send.finish()?;
            conn.closed().await;
            return Ok(());
        }
    };
    let alias = who.alias.clone();

    // A tester may plead more than once on one connection, and releases on it too.
    while let Some(plea) = read_line::<Plea>(&mut recv).await? {
        // Each arm produces a decision *or* a fault. Only the pleas that need a policy
        // read can fault; the rest are answered from state we already hold.
        let decided: Result<Verdict> = match plea {
            Plea::Ping => {
                println!("{alias} pinged us");
                Ok(Verdict::Pong)
            }
            Plea::Inspect { what } => match &who.admin {
                Some(name) => {
                    println!("{name} looked at {what:?}");
                    Ok(Verdict::Saw(control.look(what).await))
                }
                // Said plainly rather than pretending the command does not exist.
                // Being one of our own devices is not enough — a worker is one of
                // those — so the message names what is actually required.
                None => Ok(Verdict::Deny {
                    rationale: "not an admin of this controller — see `cm admin add`".into(),
                }),
            },
            Plea::Nivedana(nivedana) => {
                decide(&control, &alias, &caller, &who.attested, &nivedana).await
            }
            Plea::Rehearse(nivedana) => rehearse(&control, &alias, &who.attested, &nivedana).await,
            Plea::Upload(up) => Ok(control.accept_policy(&alias, &up).await),
            Plea::Enrol { note } => {
                Ok(control.enrol(&alias, &caller, &knock, note.as_deref()).await)
            }
            Plea::Forget { key } => Ok(control.forget(&alias, &caller, key.as_deref()).await),
            Plea::Whoami => {
                let mut lines = vec![format!("you are {alias}")];
                if !who.attested.is_empty() {
                    for (k, v) in &who.attested {
                        lines.push(format!("  {k}: {v}"));
                    }
                }
                let held = control.enrolled.lock().await;
                let mine = held.held_by(&alias);
                if mine.is_empty() {
                    lines.push("no enrolled keys — this connection proved itself another way".into());
                } else {
                    for e in mine {
                        let mark = if e.key == caller { " (this one)" } else { "" };
                        lines.push(format!(
                            "  {} via {}{mark}{}",
                            e.key,
                            e.issuer,
                            e.note.as_deref().map(|n| format!(" — {n}")).unwrap_or_default()
                        ));
                    }
                }
                Ok(Verdict::Saw(proto::Sight { lines }))
            }
            Plea::Release { reservation } => {
                let closed = control.fleet.lock().await.release(&reservation, &caller);
                match closed {
                    Ok(closed) => {
                        println!(
                            "{alias} released {reservation} ({} machine(s), {} credit(s))",
                            closed.workers().len(),
                            closed.credits
                        );
                        control.charge(&closed);
                        control.tell_freed(reservation.as_str(), closed.workers()).await;
                        Ok(Verdict::Ok)
                    }
                    Err(reason) => Ok(Verdict::Deny { rationale: reason }),
                }
            }
        };

        match decided {
            Ok(verdict) => write_line(&mut send, &Answer::Decided(verdict)).await?,
            Err(fault) => {
                // Stop. A controller that could not weigh this plea cannot weigh the
                // next one either, and serving further requests on this connection
                // would be pretending otherwise. The caller is told plainly first,
                // because a silently dropped stream in CI output is indistinguishable
                // from a network problem.
                eprintln!("could not answer {alias}: {fault:#}");
                write_line(&mut send, &Answer::Fault { fault: format!("{fault:#}") }).await?;
                // Then wait for *them* to hang up. Returning here would drop the
                // connection an instant after the write and take the frame with it —
                // the caller saw "connection lost: closed by peer" and never learned
                // why, which is the one thing this whole path exists to tell them.
                send.finish()?;
                conn.closed().await;
                return Ok(());
            }
        }
    }
    send.finish()?;
    Ok(())
}

/// Who is this, if we should talk to them at all?
///
/// The ticket is the only source: we hold no `network.toml` and have never heard of
/// the caller. Verifying our parent's signature is the whole of it.
async fn admit(
    control: &Control,
    knock: &Knock,
    caller: &sirji::PublicKey,
) -> Result<Caller, String> {
    let key = id52::encode(caller);

    // A ticket first, because it is the cheaper check and the common one: one signature,
    // no network, nothing cached. An attestation is for callers that had nothing to enrol.
    if let Some(ticket) = &knock.ticket {
        if let Err(e) = ticket.verify(caller, &control.config.parent) {
            return Err(format!("{e:#}"));
        }
        if ticket.name != control.config.name {
            return Err(format!("that ticket is for `{}`", ticket.name));
        }
        return Ok(Caller::from(ticket.alias.clone()).with_admin(&control.admins, &key));
    }

    // A key we already agreed to remember. No token, no expiry, nothing to refresh: the
    // connection is the credential, because dialling from a key is possession of it. This
    // is checked before the attestation path so an enrolled machine never pays for a
    // browser round trip it does not need.
    if knock.ticket.is_none()
        && let Some(known) = control.enrolled.lock().await.who(&key)
    {
        return Ok(Caller::enrolled(known.alias.clone(), known.issuer.clone()));
    }

    if let Some(token) = &knock.attestation {
        if control.issuers.is_empty() {
            return Err(format!(
                "this controller trusts no issuers, so an attestation proves nothing here \
                 — the host lists them in {}",
                attest::FILE
            ));
        }
        // The caller's own connection key is the audience the token must name. Everything
        // about replay protection is that comparison, and it happens inside `verify`.
        return match control.issuers.verify(token, &key).await {
            // Never an admin. Admin membership is by key, from a list the host wrote by
            // hand, and an attested caller's key is one it minted for this run.
            Ok(vouched) => Ok(Caller::attested(vouched.alias, vouched.facts)),
            Err(e) => Err(format!("{e:#}")),
        };
    }

    Err("no proof of who you are — resolve me as `name@org`, or send an attestation".into())
}

/// Who is on the other end, and what class of thing they are.
pub struct Caller {
    /// For display and for policy. Peers have a name; a sibling device does not.
    alias: String,
    /// What an issuer vouched for, when one did: the repository, the ref, the workflow.
    /// Proven, so it joins the deployment's own facts in the prompt's attested half —
    /// and a policy may turn on it, because the caller could not have said it.
    attested: BTreeMap<String, String>,
    /// A device of our **own** sirji rather than somebody else's peer.
    sibling: bool,
    /// On the admin list. Strictly narrower than `sibling`: a worker is a sibling
    /// too, and a machine that offers capacity has no business reading the roster,
    /// every reservation, or anybody's budget.
    admin: Option<String>,
}

impl Caller {
    /// The whole discriminator is whether the ticket carried an alias, and it is
    /// sound for a reason worth writing down because it is load-bearing:
    ///
    /// our parent mints tickets two ways. A peer resolving us through `ResolveFor`
    /// gets `alias = Some(their name)` — necessarily, because an alias is how a
    /// `[[peer]]` is keyed in `network.toml` and one without a name cannot be looked
    /// up. A sibling device resolving through `ResolveLocal` gets `alias = None`,
    /// because there is no person behind a device.
    ///
    /// So no alias implies a device of our own organisation. If sirji ever mints an
    /// aliasless ticket for a peer, this becomes wrong, which is what the test in
    /// this file is there to catch.
    fn from(alias: Option<String>) -> Self {
        match alias {
            Some(alias) => {
                Self { alias, sibling: false, admin: None, attested: BTreeMap::new() }
            }
            None => Self {
                alias: "an unnamed peer".into(),
                sibling: true,
                admin: None,
                attested: BTreeMap::new(),
            },
        }
    }

    /// A key this controller agreed to remember, proving itself by having dialled.
    ///
    /// Never a sibling and never an admin, for the same reason an attested caller is not:
    /// those are decided against lists a host wrote by hand, and this key arrived by
    /// somebody proving an identity rather than by an operator typing it.
    fn enrolled(alias: String, issuer: String) -> Self {
        Self {
            alias,
            sibling: false,
            admin: None,
            attested: [("issuer".to_string(), issuer)].into(),
        }
    }

    /// Vouched for by an issuer rather than by our own sirji.
    ///
    /// Never a sibling and never an admin: those are decided by key, against lists the
    /// host wrote by hand, and this caller's key is one it minted for a single run.
    fn attested(alias: String, facts: BTreeMap<String, String>) -> Self {
        Self { alias, sibling: false, admin: None, attested: facts }
    }

    /// Admin membership is decided by **key**, against a list the host wrote by
    /// hand — never by being a sibling, and never by anything the caller says.
    fn with_admin(mut self, admins: &admin::Admins, key: &str) -> Self {
        if let Some(found) = admins.by_key(key).filter(|_| self.sibling) {
            self.alias = found.name.clone();
            self.admin = Some(found.name.clone());
        }
        self
    }
}

/// One decision, start to finish: what the model said, what it was cut to, and why.
///
/// Shared by the controller and `cm policy-test`, so a test cannot pass against a
/// decision the fleet would not make.
pub struct Weighed {
    /// What the model proposed, before anything was applied.
    pub said: u32,
    /// What it comes to after the ask, the ceiling, availability and the budget.
    pub allowed: u32,
    /// A refusal from the model itself, in its own words.
    pub denied: Option<String>,
    pub rationale: String,
    /// Limits it was shown and exceeded anyway. Empty in the ordinary case.
    pub faults: Vec<String>,
}

impl Weighed {
    /// The grant, or nothing — a denial, or everything clamped away.
    fn granted(&self) -> Option<(u32, String)> {
        if self.denied.is_some() || self.allowed == 0 {
            return None;
        }
        Some((self.allowed, self.rationale.clone()))
    }

    /// Why there is nothing, in cm's words rather than the model's: a sentence that
    /// argued for a number nobody is getting does not explain the one they are.
    fn refusal(&self, wanted: u32) -> String {
        if let Some(why) = &self.denied {
            return why.clone();
        }
        if self.faults.is_empty() {
            format!("this policy granted none of the {wanted} asked for")
        } else {
            self.faults.join("; ")
        }
    }

    fn log(&self, alias: &str) -> Vec<String> {
        match &self.denied {
            Some(why) => vec![format!("policy refused {alias}: {why}")],
            // Both numbers: "why did I get 4" must be answerable, and a clamp that left
            // no trace would make an organisation's own ceiling invisible.
            None => vec![format!(
                "policy weighed {alias}: said {}, giving {} — {}",
                self.said, self.allowed, self.rationale
            )],
        }
    }

    /// Loudly, when it fires. A policy that keeps overshooting is a policy nobody has
    /// noticed is wrong, because the clamp has been quietly making it look right.
    fn fault_report(&self, tenant: &str) -> Option<String> {
        if self.faults.is_empty() {
            return None;
        }
        Some(format!(
            "policy for {tenant} overshot limits it was shown ({}) — cut to {}. Fix the \
             policy; the prompt stated every one of them.",
            self.faults.join("; "),
            self.allowed
        ))
    }
}

/// Who gets to look at the answer before it is enforced.
///
/// `Option` in [`weigh`] rather than a required argument, because `cm policy-test` runs
/// against a folder with no deployment behind it — and a test that silently invented one
/// would be checking something other than the policy.
pub struct Review<'a> {
    pub directory: &'a dyn Directory,
    pub about: directory::Weighing<'a>,
}

pub async fn weigh(
    adviser: &adviser::Claude,
    advice: &adviser::Advice,
    limit: Limit,
    review: Option<Review<'_>>,
) -> Result<Weighed> {
    // No fallback, on purpose, and no verdict either — this leaves as an error. There is
    // no number to substitute that any organisation asked for, and inventing one would
    // keep the fleet running while nobody's policy is applied.
    let mut opinion = adviser.weigh(advice).await?;

    // The deployment's own look at it, before anything is enforced. An error here fails
    // the request: a decision somebody could not record is one they should not act on,
    // and deciding that for them is not cm's business.
    if let Some(review) = &review {
        review.directory.reviewed(&review.about, &mut opinion).await?;
    }

    if opinion.denies() {
        return Ok(Weighed {
            said: opinion.count,
            allowed: 0,
            denied: Some(opinion.rationale.clone()),
            rationale: opinion.rationale,
            faults: Vec::new(),
        });
    }

    let said = opinion.count;
    let (allowed, faults) = sanity(said, opinion.bounded(advice), limit);
    let rationale = if faults.is_empty() {
        opinion.rationale
    } else {
        format!("{allowed} machine(s): {}", faults.join("; "))
    };
    Ok(Weighed { said, allowed, denied: None, rationale, faults })
}

/// What policy and the fleet concluded: a number, or a verdict to send instead.
///
/// Wrapped in a `Result` by its callers, whose `Err` means something different again —
/// that nothing was concluded at all. Three outcomes, and the type says which is which:
/// `Ok(Ok(n))` you may have n, `Ok(Err(v))` here is why not, `Err(e)` nobody decided.
pub type Entitled = std::result::Result<(u32, Option<String>), Verdict>;

/// A refusal is a decision, so it is an `Ok` carrying a verdict.
fn refused(rationale: String) -> Result<Entitled> {
    Ok(Err(Verdict::Deny { rationale }))
}

/// Every bound that is re-checked after the model has answered.
///
/// All four were in the prompt. This exists for the case where the answer ignored one
/// anyway — a policy that argues past its own ceiling, a model that misreads a
/// number, a fleet that emptied between the brief and the answer.
#[derive(Clone)]
pub struct Limit {
    /// The organisation's own `max_limit`.
    pub ceiling: u32,
    /// Machines free right now.
    pub free: u32,
    /// The host's cap on this tenant, whatever their policy says.
    pub host: u32,
    /// What the budget can buy, when there is one.
    pub affordable: Option<u32>,
}

/// Cut an answer to what is actually possible, and say what had to be cut.
///
/// A returned fault is a **defect report**, not a normal outcome: every limit it names
/// was stated in the prompt, availability included, so exceeding one means the policy
/// argued past a number it was shown.
///
/// A fleet that empties *after* this is a different matter and reports nothing: the
/// brief is a snapshot, `choose` re-checks what is free at the moment of allocation,
/// and a busy afternoon is nobody's mistake.
pub fn sanity(said: u32, bounded: u32, limit: Limit) -> (u32, Vec<String>) {
    let mut faults = Vec::new();
    let mut allowed = bounded;

    if said > limit.ceiling {
        faults.push(format!("proposed {said} against a stated ceiling of {}", limit.ceiling));
    }
    if said > limit.free {
        faults.push(format!("proposed {said} with only {} machine(s) free", limit.free));
    }
    allowed = allowed.min(limit.free);

    if allowed > limit.host {
        faults.push(format!(
            "proposed {allowed} against this tenant's host ceiling of {}",
            limit.host
        ));
        allowed = limit.host;
    }
    if let Some(affordable) = limit.affordable
        && affordable < allowed
    {
        faults.push(format!("proposed {allowed}, but the budget buys {affordable}"));
        allowed = affordable;
    }
    (allowed, faults)
}

/// Policy decides entitlement; the fleet decides availability. Both must agree.
///
/// Answer "what would I get", holding nothing.
///
/// Runs exactly the decision a real plea runs — the same policy, and the fleet's own
/// selection through `choose` — and stops before the commit. Sharing `choose` rather
/// than estimating is the whole point: an approximation would eventually disagree
/// with the real path, and be believed.
pub async fn rehearse(
    control: &Control,
    alias: &str,
    vouched: &BTreeMap<String, String>,
    nivedana: &Nivedana,
) -> Result<Verdict> {
    // `?` on the outer result: a plea nobody could weigh leaves as an error and never
    // becomes a verdict. The inner one is a real decision, so it is returned as one.
    let (allowed, rationale) = match control.entitlement(alias, vouched, nivedana).await? {
        Ok(pair) => pair,
        Err(refusal) => return Ok(refusal),
    };

    let chosen = control
        .fleet
        .lock()
        .await
        .choose(&nivedana.capabilities, allowed);

    println!("{alias} asked what they would get");
    Ok(match chosen {
        Ok(workers) => Verdict::Would {
            count: workers.len() as u32,
            rationale: rationale.unwrap_or_else(|| "as things stand".into()),
        },
        Err(Shortfall::NoneCapable { wanted }) => Verdict::Deny {
            rationale: format!(
                "no machine in the fleet can do {wanted:?} — waiting will not change that"
            ),
        },
        Err(Shortfall::Fewer { available }) => Verdict::Would {
            count: available,
            rationale: format!(
                "you may have up to {allowed}, and {available} matching machine(s) are free right now"
            ),
        },
    })
}

pub async fn decide(
    control: &Control,
    alias: &str,
    caller: &str,
    vouched: &BTreeMap<String, String>,
    nivedana: &Nivedana,
) -> Result<Verdict> {
    // `?` on the outer result: a plea nobody could weigh leaves as an error and never
    // becomes a verdict. The inner one is a real decision, so it is returned as one.
    let (allowed, rationale) = match control.entitlement(alias, vouched, nivedana).await? {
        Ok(pair) => pair,
        Err(refusal) => return Ok(refusal),
    };
    let lifetime = control.lifetime_for(alias).await;
    // Who pays. Not the caller's alias: several callers may share one tenant.
    let tenant_name = control.tenant_of(alias).await;

    let allocation = {
        let mut guard = control.fleet.lock().await;
        match guard.allocate(nivedana, allowed, caller, alias, &tenant_name, lifetime) {
            Ok(allocation) => allocation,
            Err(Shortfall::NoneCapable { wanted }) => {
                return Ok(Verdict::Deny {
                    rationale: format!(
                        "no machine in the fleet can do {wanted:?} — waiting will not change that"
                    ),
                });
            }
            Err(Shortfall::Fewer { available }) => {
                return Ok(Verdict::Counter {
                    count: available,
                    rationale: format!(
                        "you may have up to {allowed}, but {available} matching machine(s) are free"
                    ),
                });
            }
        }
    };

    let workers: Vec<WorkerHandle> = allocation
        .workers
        .iter()
        .map(|w| WorkerHandle {
            name: w.name.clone(),
            key: w.key.clone(),
            hints: w.hints.clone(),
            ticket: sirji::Ticket::mint(
                &control.signing,
                w.name.clone(),
                caller,
                Some(alias.to_string()),
                allocation.expires_in,
            ),
        })
        .collect();

    // Tell the machines before telling the caller, so none is ever dialled for a
    // reservation it has not yet heard of.
    {
        let orders = control.orders.lock().await;
        for w in &workers {
            if let Some(tx) = orders.get(&w.name) {
                let _ = tx
                    .send(Aadesh::Assigned {
                        reservation: allocation.reservation.clone(),
                        caller: caller.to_string(),
                        limits: allocation.limits,
                    })
                    .await;
            }
        }
    }

    println!(
        "{alias}: granted {} machine(s) as {}",
        workers.len(),
        allocation.reservation
    );
    Ok(Verdict::Grant {
        reservation: allocation.reservation,
        workers,
        expires_in: allocation.expires_in,
        rationale,
    })
}

async fn serve_worker(
    control: Arc<Control>,
    conn: sirji::Connection,
    mut send: sirji::SendStream,
    register: Register,
    key: String,
) -> Result<()> {
    let name = register.name.clone();
    println!(
        "worker {name} arrived: {} credit(s)/min, can {:?}",
        register.rate.max(1), register.capabilities
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Aadesh>(16);
    control.orders.lock().await.insert(name.clone(), tx);
    control.fleet.lock().await.arrive(Worker {
        name: name.clone(),
        key,
        hints: register.hints,
        capabilities: register.capabilities,
        // A machine of unknown cost must not be free, so silence means one.
        rate: register.rate.max(1),
        held_by: None,
    });

    // Push orders down the registration stream for as long as it is open. When it
    // closes the worker is gone — the connection is the availability.
    loop {
        tokio::select! {
            order = rx.recv() => match order {
                Some(order) => {
                    if write_line(&mut send, &order).await.is_err() {
                        break;
                    }
                }
                None => break,
            },
            _ = conn.closed() => break,
        }
    }

    control.orders.lock().await.remove(&name);
    control.fleet.lock().await.depart(&name);
    println!("worker {name} gone");
    Ok(())
}


// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn a_key() -> String {
        sirji::id52::encode(&sirji::SecretKey::generate().public())
    }

    fn limit() -> Limit {
        Limit { ceiling: 10, free: 8, host: 6, affordable: Some(5) }
    }

    #[test]
    fn an_answer_inside_every_limit_is_left_alone_and_reports_nothing() {
        // The ordinary case. A fault list that is never empty is a fault list nobody
        // reads, so staying silent when there is nothing wrong is the whole point.
        let (allowed, faults) = sanity(4, 4, limit());
        assert_eq!(allowed, 4);
        assert!(faults.is_empty(), "{faults:?}");
    }

    #[test]
    fn overshooting_a_limit_that_was_in_the_prompt_is_reported_as_a_fault() {
        // Every one of these was stated to the model. Hitting one is not the system
        // working as designed — it means the policy argues past its own numbers, and
        // somebody has to be told rather than have the clamp hide it.
        let (allowed, faults) = sanity(99, 99, limit());
        assert_eq!(allowed, 5, "cut to what the budget buys");
        assert!(faults.iter().any(|f| f.contains("stated ceiling of 10")), "{faults:?}");
        assert!(faults.iter().any(|f| f.contains("only 8 machine(s) free")), "{faults:?}");
        assert!(faults.iter().any(|f| f.contains("host ceiling of 6")), "{faults:?}");
        assert!(faults.iter().any(|f| f.contains("budget buys 5")), "{faults:?}");
    }

    #[test]
    fn promising_machines_it_was_told_were_busy_is_a_fault_too() {
        // The free count is in the prompt like every other limit, so proposing past it
        // is the same class of defect as proposing past the ceiling — not the fleet
        // having moved, which happens later and reports nothing.
        let (allowed, faults) =
            sanity(3, 3, Limit { ceiling: 10, free: 1, host: 6, affordable: None });
        assert_eq!(allowed, 1);
        assert!(faults.iter().any(|f| f.contains("only 1 machine(s) free")), "{faults:?}");
        assert!(faults.iter().all(|f| !f.contains("host")), "{faults:?}");
    }

    #[test]
    fn availability_wins_over_everything_policy_permits() {
        // Granting machines that are not there is the one error a caller cannot work
        // around: they would be handed a reservation the fleet cannot honour.
        let (allowed, _) =
            sanity(6, 6, Limit { ceiling: 100, free: 0, host: 100, affordable: Some(100) });
        assert_eq!(allowed, 0);
    }

    #[test]
    fn a_zero_from_the_model_is_explained_by_the_faults_not_the_model() {
        // A "granted what you asked for" sentence printed beside a refusal is worse
        // than no reason at all: it describes the argument, not the outcome.
        let (allowed, faults) =
            sanity(0, 0, Limit { ceiling: 10, free: 8, host: 6, affordable: Some(5) });
        assert_eq!(allowed, 0);
        assert!(faults.is_empty(), "nothing overshot — the model simply said none");
    }

    #[test]
    fn a_tenant_with_no_budget_is_not_treated_as_having_none() {
        // `None` means unmetered, and reading it as zero would refuse every request
        // from every self-hosted tenant that never set a budget.
        let (allowed, faults) =
            sanity(6, 6, Limit { ceiling: 10, free: 8, host: 10, affordable: None });
        assert_eq!(allowed, 6);
        assert!(faults.is_empty(), "{faults:?}");
    }

    #[test]
    fn an_attested_caller_is_never_a_sibling_or_an_admin() {
        // Both of those are decided by key, against lists a host wrote by hand. An
        // attested caller's key is one it minted for a single run, so it can never be on
        // either list — and a token that claimed otherwise must not change that.
        let who = Caller::attested(
            "github:acme/payments".into(),
            [("repository".to_string(), "acme/payments".to_string())].into(),
        );
        assert!(!who.sibling, "an attested caller is not one of our own devices");
        assert!(who.admin.is_none(), "and never an admin");
        assert_eq!(who.alias, "github:acme/payments");
        assert_eq!(who.attested.len(), 1);

        // Even asked to become one, against a list that names its key.
        let key = a_key();
        let admins = admin::Admins { list: vec![admin::Admin {
            name: "ops".into(),
            key: key.clone(),
            note: None,
        }] };
        let still = Caller::attested("github:acme/payments".into(), Default::default())
            .with_admin(&admins, &key);
        assert!(still.admin.is_none(), "attestation is not a route to admin");
    }

    #[test]
    fn a_peer_is_never_a_sibling() {
        // Our parent mints an alias for every peer, because an alias is how a
        // [[peer]] is keyed and one without a name could not be looked up; a sibling
        // device has no person behind it and gets None. If sirji ever mints an
        // aliasless ticket for a peer, this fails — which is exactly when somebody
        // needs to know.
        assert!(Caller::from(None).sibling, "a device of ours");
        assert!(!Caller::from(Some("dana".into())).sibling, "a named peer is not");
        assert_eq!(Caller::from(Some("dana".into())).alias, "dana");
    }

    #[test]
    fn being_one_of_ours_does_not_make_you_an_admin() {
        // The flaw this class exists to fix: every worker is a sibling device, and a
        // machine offering capacity must not be able to read the roster, every live
        // reservation, or anybody's budget.
        let mut admins = admin::Admins::default();
        let ops = a_key();
        let worker = a_key();
        admins
            .add(admin::Admin { name: "ops".into(), key: ops.clone(), note: None })
            .unwrap();

        assert_eq!(
            Caller::from(None).with_admin(&admins, &ops).admin.as_deref(),
            Some("ops")
        );
        assert!(
            Caller::from(None).with_admin(&admins, &worker).admin.is_none(),
            "a sibling not on the list is not an admin"
        );
    }

    #[test]
    fn a_peer_cannot_become_an_admin_by_sharing_a_key() {
        // Belt and braces: even if a peer's key somehow appeared on the admin list,
        // arriving with an alias means arriving as somebody else's peer, and that is
        // not the door admins come through.
        let mut admins = admin::Admins::default();
        let key = a_key();
        admins
            .add(admin::Admin { name: "ops".into(), key: key.clone(), note: None })
            .unwrap();
        assert!(
            Caller::from(Some("dana".into())).with_admin(&admins, &key).admin.is_none()
        );
    }
}
