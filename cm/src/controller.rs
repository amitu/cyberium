//! The controller: everything that has a view of the whole fleet.
//!
//! Public because a deployment with its own identity service builds its own binary
//! around this rather than reimplementing the protocol.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;

use crate::fleet::{Fleet, Shortfall, Worker};
use crate::policy::Ruling;
use crate::proto::{Aadesh, Answer, Knock, Nivedana, Plea, Register, Verdict, WorkerHandle};
use crate::{admin, adviser, budget, fleet, proto, tenant, upload};
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
    tenants: Mutex<tenant::Tenants>,
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

pub async fn controller() -> Result<()> {
    let home = home()?;
    sirji::Settings::load(&home)?.activate();
    let config = load_config(&home)?;

    // Every tenant is read and validated here, not lazily: a policy that does not
    // parse should stop the controller starting, rather than surface an hour later
    // on somebody's first plea with them waiting on the other end.
    let tenants = tenant::Tenants::load(&config.root)?;
    println!("controller `{}` listening as {}", config.name, config.key);
    if tenants.is_empty() {
        // Not an error — a controller with no tenants is correctly configured for
        // nobody. Said loudly because every plea will be refused until this is
        // fixed, and the refusal alone would not explain why.
        println!(
            "no tenants in {} — every plea will be refused.\n  add one with `cm tenant add <alias>`",
            tenant::Tenants::dir_in(&config.root).display()
        );
    } else {
        println!(
            "{} tenant(s): {}",
            tenants.len(),
            tenants.names().cloned().collect::<Vec<_>>().join(", ")
        );
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
        tenants: Mutex::new(tenants),
        admins,
        adviser,
        orders: Mutex::new(Default::default()),
        // Tickets admitting a caller to a worker are signed by us, and verified by
        // the worker against the controller key it registered with. The mechanism
        // sirji uses one level up, reused one level down.
        signing: secret,
    });

    let hints = listening(&endpoint).await;
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
        nivedana: &Nivedana,
    ) -> Result<Entitled> {
        // Everything the decision needs, gathered before anything slow happens — and
        // every lock released before the model call. A 30s network round trip under
        // the tenants or fleet mutex would serialise every plea in the fleet behind
        // one caller's request.
        let (
            tenant_name,
            facts,
            rulebook,
            lifetime,
            host_ceiling,
            terms,
            standing,
            ceiling,
            wanted,
        ) = {
            let mut tenants = self.tenants.lock().await;
            let Some(tenant) = tenants.for_caller(alias) else {
                // Named, but not onboarded. Distinguished from a policy refusal
                // because the fix is completely different — somebody has to run
                // `cm tenant add`.
                return refused(format!("{alias} is not a tenant of this controller"));
            };

            // Authorisation is deterministic; the number is not. How many machines a
            // plea deserves is the question the policy was written to answer, so it is
            // read for every request rather than only the large ones.
            let (wanted, standing, ceiling) = match tenant.policy.weigh(alias, nivedana) {
                Ruling::Deny { rationale } => return refused(rationale),
                Ruling::Consider { wanted, standing, ceiling } => (wanted, standing, ceiling),
            };
            (
                tenant.alias.clone(),
                tenant.facts(),
                tenant.rulebook.as_str().to_string(),
                tenant.policy.reservation_secs(),
                tenant.ceiling(),
                tenant.budget(),
                standing,
                ceiling,
                wanted,
            )
        };

        // An empty instructions block is the one input that makes a model invent a rule
        // rather than apply one, so it is refused rather than sent. Reachable only if
        // somebody emptied the folder — `cm tenant add` writes a starter policy.
        if rulebook.trim().is_empty() {
            return refused(format!(
                "tenant `{tenant_name}` has nothing written down — no policy to weigh \
                 this against"
            ));
        }

        let dir = tenant::Tenants::dir_in(&self.config.root).join(&tenant_name);
        let now = fleet::now();
        let (brief, money) = {
            let fleet = self.fleet.lock().await;
            let brief = fleet.brief(&nivedana.capabilities);
            let money = terms.map(|(budget, window_secs)| adviser::Money {
                budget,
                spent: budget::spent(&dir, now, window_secs),
                committed: fleet.committed(&tenant_name),
                window_secs,
            });
            (brief, money)
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
                facts,
            },
            declared: adviser::Declared {
                said: nivedana.said.clone(),
                count: wanted,
                capabilities: nivedana.capabilities.clone(),
            },
            standing,
            // The tightest ceiling that applies, not just the tenant's own. Springing
            // the host's cap after the fact would make a model argue for a number it
            // was never allowed to give, and the caller read a rationale for a
            // decision that did not happen.
            ceiling: ceiling.min(host_ceiling),
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
            ceiling: ceiling.min(host_ceiling),
            free: brief.free,
            host: host_ceiling,
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
        let weighed = weigh(&self.adviser, &advice, limit)
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

    /// What this tenant's budget still allows.
    ///
    /// The lower of two windows, like the two ceilings: the host's in `tenant.toml`
    /// and the tenant's own in `policy.md`. Both rolling seconds, so neither needs a
    /// calendar or a timezone.
    async fn room(&self, tenant: &tenant::Tenant) -> Option<budget::Room> {
        let (budget_credits, window) = tenant.budget()?;
        let dir = tenant::Tenants::dir_in(&self.config.root).join(&tenant.alias);
        let now = fleet::now();
        Some(budget::Room {
            budget: budget_credits,
            spent: budget::spent(&dir, now, window),
            committed: self.fleet.lock().await.committed(&tenant.alias),
        })
    }

    /// Which tenant this caller bills to.
    /// Take a policy folder from one of a tenant's admins.
    ///
    /// A verdict rather than an error: every outcome here is a decision about the
    /// caller's authority or their files, and nothing about it is undecidable.
    async fn accept_policy(&self, alias: &str, up: &proto::Upload) -> Verdict {
        let mut tenants = self.tenants.lock().await;
        let Some(tenant) = tenants.for_caller(alias) else {
            return Verdict::Deny {
                rationale: format!("{alias} is not a tenant of this controller"),
            };
        };
        let name = tenant.alias.clone();
        if !tenant.may_write(alias) {
            // Named, not authorised. Said as two different things because the fix is
            // different: one needs onboarding, the other needs somebody with the right.
            let who = tenant.admins();
            return Verdict::Deny {
                rationale: if who.is_empty() {
                    format!(
                        "tenant `{name}` has no admins, so nobody may change its policy — \
                         the host sets them in {}",
                        tenant::FILE
                    )
                } else {
                    format!(
                        "{alias} may run tests for `{name}` but not change its policy. \
                         Ask one of: {}",
                        who.join(", ")
                    )
                },
            };
        }
        drop(tenants);

        let dir = tenant::Tenants::dir_in(&self.config.root).join(&name);
        match upload::accept(&dir, up) {
            Err(e) => {
                eprintln!("refused a policy for {name} from {alias}: {e:#}");
                Verdict::Deny { rationale: format!("{e:#}") }
            }
            Ok(written) => {
                // Re-read at once. A controller still weighing pleas against the folder
                // it replaced would be applying a policy that no longer exists.
                let mut tenants = self.tenants.lock().await;
                if let Err(e) = tenants.rescan() {
                    eprintln!("wrote {name}'s policy but could not reload it: {e:#}");
                    return Verdict::Deny {
                        rationale: format!("written, but this controller could not load it: {e}"),
                    };
                }
                println!(
                    "{alias} replaced {name}'s policy: {} file(s) — {}",
                    written.len(),
                    written.join(", ")
                );
                Verdict::Ok
            }
        }
    }

    async fn tenant_of(&self, alias: &str) -> String {
        self.tenants
            .lock()
            .await
            .for_caller(alias)
            .map(|t| t.alias.clone())
            .unwrap_or_else(|| alias.to_string())
    }

    /// How long this tenant's grants last unreleased.
    async fn lifetime_for(&self, alias: &str) -> u64 {
        self.tenants
            .lock()
            .await
            .for_caller(alias)
            .map(|t| t.policy.reservation_secs())
            .unwrap_or(fleet::RESERVATION_SECS)
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
                let names: Vec<String> = {
                    let tenants = self.tenants.lock().await;
                    tenants.names().cloned().collect()
                };
                if names.is_empty() {
                    lines.push("no tenants".into());
                }
                // First, because it changes how to read everything under it: a tenant
                // listed here is running on terms that are not the ones in its file.
                for (name, why) in {
                    let tenants = self.tenants.lock().await;
                    tenants
                        .unread()
                        .into_iter()
                        .map(|(n, w)| (n.to_string(), w.to_string()))
                        .collect::<Vec<_>>()
                } {
                    lines.push(format!(
                        "  !! {name}: files not read, still running on the last good copy \
                         — {why}"
                    ));
                }
                for name in names {
                    let t = {
                        let tenants = self.tenants.lock().await;
                        tenants.by_name(&name).cloned()
                    };
                    let Some(t) = t else { continue };
                    match self.room(&t).await {
                        Some(room) => lines.push(format!(
                            "  {:<16} {} of {} credit(s) used, {} committed, {} left",
                            t.alias, room.spent, room.budget, room.committed, room.left()
                        )),
                        // No budget is a real state, and saying "0 of 0" would read
                        // as a tenant that may spend nothing.
                        None => lines.push(format!("  {:<16} no budget set", t.alias)),
                    }
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

    let who = match admit(&control, &knock, &caller_key) {
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
            Plea::Nivedana(nivedana) => decide(&control, &alias, &caller, &nivedana).await,
            Plea::Rehearse(nivedana) => rehearse(&control, &alias, &nivedana).await,
            Plea::Upload(up) => Ok(control.accept_policy(&alias, &up).await),
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
fn admit(control: &Control, knock: &Knock, caller: &sirji::PublicKey) -> Result<Caller, String> {
    let Some(ticket) = &knock.ticket else {
        return Err("no ticket — resolve me as `name@org`".into());
    };
    if let Err(e) = ticket.verify(caller, &control.config.parent) {
        return Err(format!("{e:#}"));
    }
    if ticket.name != control.config.name {
        return Err(format!("that ticket is for `{}`", ticket.name));
    }
    Ok(Caller::from(ticket.alias.clone()).with_admin(&control.admins, &id52::encode(caller)))
}

/// Who is on the other end, and what class of thing they are.
pub struct Caller {
    /// For display and for policy. Peers have a name; a sibling device does not.
    alias: String,
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
            Some(alias) => Self { alias, sibling: false, admin: None },
            None => Self { alias: "an unnamed peer".into(), sibling: true, admin: None },
        }
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

pub async fn weigh(
    adviser: &adviser::Claude,
    advice: &adviser::Advice,
    limit: Limit,
) -> Result<Weighed> {
    // No fallback, on purpose, and no verdict either — this leaves as an error. There is
    // no number to substitute that any organisation asked for, and inventing one would
    // keep the fleet running while nobody's policy is applied.
    let opinion = adviser.weigh(advice).await?;

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
pub async fn rehearse(control: &Control, alias: &str, nivedana: &Nivedana) -> Result<Verdict> {
    // `?` on the outer result: a plea nobody could weigh leaves as an error and never
    // becomes a verdict. The inner one is a real decision, so it is returned as one.
    let (allowed, rationale) = match control.entitlement(alias, nivedana).await? {
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
    nivedana: &Nivedana,
) -> Result<Verdict> {
    // `?` on the outer result: a plea nobody could weigh leaves as an error and never
    // becomes a verdict. The inner one is a real decision, so it is returned as one.
    let (allowed, rationale) = match control.entitlement(alias, nivedana).await? {
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
