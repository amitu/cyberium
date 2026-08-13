//! `cm` — cost-aware allocation of test machines, over sirji.
//!
//! Three roles, all sirji **devices**, none holding any identity state:
//!
//! - **`cm controller`** answers to a name at an organisation's sirji. It owns the
//!   whole picture: which machines are here, what they can do, who has them, and
//!   when to take them back.
//! - **`cm worker`** offers capacity. It finds the controller through their shared
//!   parent, registers, and holds the connection — that connection *is* its
//!   availability.
//! - **`cm test`** is a device of the developer's own sirji. It resolves the
//!   controller, pleads, then talks to the granted machines **directly**.
//!
//! The controller allocates; it never carries the work. Workers never speak to each
//! other — they have nothing to say, because everything needing a view of the whole
//! fleet lives in exactly one place.

mod fleet;
mod policy;
mod proto;

use std::sync::Arc;

use anyhow::{Result, bail};
use fleet::{Fleet, Shortfall, Worker};
use policy::Ruling;
use proto::{Aadesh, Limits, Nivedana, Outcome, Plea, Register, Upadesh, Verdict, WorkerHandle};
use sirji::id52;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

const USAGE: &str = "\
cm — cost-aware allocation of test machines

  cm init --parent <invite> [--root <dir>]
        create $CM_HOME and enrol with the sirji that issued the invite.
        Get one with `sirji device invite <name>`.

  cm controller
        own the fleet: availability, allocation, timeouts

  cm worker [--controller <name>] [--slots N] [--can <cap>]...
        offer this machine to the controller

  cm test <name@org> \"<why>\" [--count N] [--need <cap>]... [--run <cmd>]
        ask for machines, use them, and give them back
        --abandon  keep the grant and walk away, to watch it time out

$CM_HOME defaults to ~/.cm. A device has its own home because a device may be on
another machine.";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    match args.as_slice() {
        [] | ["-h"] | ["--help"] | ["help"] => {
            println!("{USAGE}");
            Ok(())
        }
        ["init", rest @ ..] => rt()?.block_on(init(rest)),
        ["controller"] => rt()?.block_on(controller()),
        ["worker", rest @ ..] => rt()?.block_on(worker(rest)),
        ["test", target, why, rest @ ..] => rt()?.block_on(test(target, why, rest)),
        _ => {
            eprintln!("{USAGE}");
            bail!("unrecognised command: {}", args.join(" "));
        }
    }
}

fn rt() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Runtime::new()?)
}

// ---------------------------------------------------------------------------
// where a device lives
// ---------------------------------------------------------------------------

const HOME_ENV: &str = "CM_HOME";
const HOME_DEFAULT: &str = ".cm";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Config {
    name: String,
    key: String,
    parent: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    parent_hints: Vec<String>,
    #[serde(default)]
    root: std::path::PathBuf,
}

fn home() -> Result<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os(HOME_ENV) {
        return Ok(std::path::PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| anyhow::anyhow!("neither CM_HOME nor HOME is set"))?;
    Ok(std::path::PathBuf::from(home).join(HOME_DEFAULT))
}

fn config_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join("cm.toml")
}

fn load_config(home: &std::path::Path) -> Result<Config> {
    Ok(toml::from_str(&std::fs::read_to_string(config_path(home))?)?)
}

fn keys(home: &std::path::Path) -> sirji::Keystore {
    sirji::Keystore::at(home.join("keys"))
}

/// Where an endpoint is reachable, for the parent to hand on.
fn listening(endpoint: &sirji::Endpoint) -> Vec<String> {
    sirji::endpoint::reachable_at(endpoint)
}

async fn write_line<T: serde::Serialize>(send: &mut sirji::SendStream, value: &T) -> Result<()> {
    send.write_all(format!("{}\n", serde_json::to_string(value)?).as_bytes())
        .await?;
    Ok(())
}

async fn read_line<T: serde::de::DeserializeOwned>(
    recv: &mut BufReader<sirji::RecvStream>,
) -> Result<Option<T>> {
    let mut line = String::new();
    if recv.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(line.trim())?))
}

/// Errors that are how the transport reports normal endings, not faults.
///
/// A caller that finished hangs up; iroh races several paths to a machine and
/// abandons the ones that lose. Printing those trains everyone to ignore the log,
/// which is worse than printing nothing.
fn quiet(e: &anyhow::Error) -> bool {
    let text = format!("{e:#}");
    text.contains("closed by peer")
        || text.contains("connection closed")
        || text.contains("during the handshake")
}

// ---------------------------------------------------------------------------
// enrolment
// ---------------------------------------------------------------------------

async fn init(args: &[&str]) -> Result<()> {
    let mut invite = None;
    let mut root = None;
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--parent" => {
                invite = args.get(i + 1).copied();
                i += 2;
            }
            "--root" => {
                root = args.get(i + 1).copied();
                i += 2;
            }
            other => bail!("unrecognised argument: {other}"),
        }
    }
    let invite = sirji::proto::Invite::decode(
        invite.ok_or_else(|| anyhow::anyhow!("--parent <invite> is required"))?,
    )?;

    let home = home()?;
    if config_path(&home).exists() {
        bail!("{} already exists", config_path(&home).display());
    }
    std::fs::create_dir_all(&home)?;

    let store = keys(&home);
    let key = store.generate()?;
    let secret = store.secret(&key)?;
    let root = root
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.clone());
    std::fs::create_dir_all(&root)?;

    println!("enrolling as {}", id52::encode(&key));
    let endpoint = sirji::endpoint::bind_dialer(secret).await?;
    let welcome = sirji::daemon::greet_parent(
        &endpoint,
        &invite,
        sirji::proto::Hello::Invited {
            invited_to: invite.identity.clone(),
            addresses: vec![id52::encode(&key)],
            dns: Vec::new(),
            // Nothing to offer yet: we report where we listen each time we
            // register, which is the version that never goes stale.
            hints: Vec::new(),
        },
    )
    .await?;
    endpoint.close().await;

    let name = match welcome {
        sirji::proto::Welcome::Ok { alias, .. } => alias,
        sirji::proto::Welcome::No { reason } => bail!("the parent refused us: {reason}"),
    };

    let config = Config {
        name: name.clone(),
        key: id52::encode(&key),
        parent: invite.addresses.clone(),
        parent_hints: invite.hints.clone(),
        root,
    };
    std::fs::write(config_path(&home), toml::to_string_pretty(&config)?)?;
    println!("enrolled as `{name}`");
    Ok(())
}

// ---------------------------------------------------------------------------
// the controller
// ---------------------------------------------------------------------------

/// Everything the controller owns.
///
/// One lock over the fleet, because allocation has to be atomic across the roster
/// and the reservations together — two locks here is how a machine gets promised
/// to two callers.
struct Control {
    config: Config,
    policy: policy::Policy,
    fleet: Mutex<Fleet>,
    /// A channel per registered worker, for pushing orders down its open stream.
    orders: Mutex<std::collections::BTreeMap<String, tokio::sync::mpsc::Sender<Aadesh>>>,
    signing: sirji::SecretKey,
}

async fn controller() -> Result<()> {
    let home = home()?;
    sirji::Settings::load(&home)?.activate();
    let config = load_config(&home)?;

    // Read policy at startup so a broken file fails here, not on the first plea
    // with somebody waiting on it.
    let policy = policy::Policy::load(&config.root)?;
    println!("controller `{}` listening as {}", config.name, config.key);
    println!(
        "policy {} — grants last {}s unreleased",
        policy.path.display(),
        policy.reservation_secs()
    );

    let secret = keys(&home).secret(&id52::decode(&config.key)?)?;
    let endpoint = sirji::bind(secret.clone()).await?;

    let control = Arc::new(Control {
        fleet: Mutex::new(Fleet::lasting(policy.reservation_secs())),
        config: config.clone(),
        policy,
        orders: Mutex::new(Default::default()),
        // Tickets admitting a caller to a worker are signed by us, and verified by
        // the worker against the controller key it registered with. The mechanism
        // sirji uses one level up, reused one level down.
        signing: secret,
    });

    let hints = listening(&endpoint);
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
        for (reservation, freed) in expired {
            println!(
                "reservation {reservation} expired unreleased, taking back {} machine(s)",
                freed.len()
            );
            control.tell_freed(&reservation, &freed).await;
        }
    }
}

impl Control {
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

async fn serve(incoming: sirji::Incoming, control: Arc<Control>) -> Result<()> {
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

    let alias = match admit(&control, &knock, &caller_key) {
        Ok(alias) => alias,
        Err(rationale) => {
            write_line(&mut send, &Verdict::Deny { rationale }).await?;
            send.finish()?;
            conn.closed().await;
            return Ok(());
        }
    };

    // A tester may plead more than once on one connection, and releases on it too.
    while let Some(plea) = read_line::<Plea>(&mut recv).await? {
        let verdict = match plea {
            Plea::Nivedana(nivedana) => decide(&control, &alias, &caller, &nivedana).await,
            Plea::Release { reservation } => {
                let freed = control.fleet.lock().await.release(&reservation, &caller);
                match freed {
                    Ok(workers) => {
                        println!(
                            "{alias} released {reservation} ({} machine(s))",
                            workers.len()
                        );
                        control.tell_freed(&reservation, &workers).await;
                        Verdict::Ok
                    }
                    Err(reason) => Verdict::Deny { rationale: reason },
                }
            }
        };
        write_line(&mut send, &verdict).await?;
    }
    send.finish()?;
    Ok(())
}

/// Who is this, if we should talk to them at all?
///
/// The ticket is the only source: we hold no `network.toml` and have never heard of
/// the caller. Verifying our parent's signature is the whole of it.
fn admit(control: &Control, knock: &Knock, caller: &sirji::PublicKey) -> Result<String, String> {
    let Some(ticket) = &knock.ticket else {
        return Err("no ticket — resolve me as `name@org`".into());
    };
    if let Err(e) = ticket.verify(caller, &control.config.parent) {
        return Err(format!("{e:#}"));
    }
    if ticket.name != control.config.name {
        return Err(format!("that ticket is for `{}`", ticket.name));
    }
    Ok(ticket
        .alias
        .clone()
        .unwrap_or_else(|| "an unnamed peer".into()))
}

/// Policy decides entitlement; the fleet decides availability. Both must agree.
async fn decide(control: &Control, alias: &str, caller: &str, nivedana: &Nivedana) -> Verdict {
    let (allowed, rationale) = match control.policy.weigh(alias, nivedana) {
        Ruling::Deny { rationale } => return Verdict::Deny { rationale },
        Ruling::Counter { count, rationale } => return Verdict::Counter { count, rationale },
        Ruling::Allow { count, rationale } => (count, rationale),
    };

    let allocation = {
        let mut guard = control.fleet.lock().await;
        match guard.allocate(nivedana, allowed, caller, alias) {
            Ok(allocation) => allocation,
            Err(Shortfall::NoneCapable { wanted }) => {
                return Verdict::Deny {
                    rationale: format!(
                        "no machine in the fleet can do {wanted:?} — waiting will not change that"
                    ),
                };
            }
            Err(Shortfall::Fewer { available }) => {
                return Verdict::Counter {
                    count: available,
                    rationale: format!(
                        "policy allows {allowed}, but {available} matching machine(s) are free"
                    ),
                };
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
    Verdict::Grant {
        reservation: allocation.reservation,
        workers,
        expires_in: allocation.expires_in,
        rationale,
    }
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
        "worker {name} arrived: {} slot(s), can {:?}",
        register.slots, register.capabilities
    );

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Aadesh>(16);
    control.orders.lock().await.insert(name.clone(), tx);
    control.fleet.lock().await.arrive(Worker {
        name: name.clone(),
        key,
        hints: register.hints,
        slots: register.slots.max(1),
        capabilities: register.capabilities,
        held_by: Vec::new(),
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
// a worker
// ---------------------------------------------------------------------------

/// Reservations this machine has been told about: which caller, within what limits.
type Assigned = Arc<Mutex<std::collections::BTreeMap<String, (String, Limits)>>>;

async fn worker(args: &[&str]) -> Result<()> {
    let mut controller_name = "cm-c".to_string();
    let mut slots = 1u32;
    let mut capabilities: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--controller" => {
                controller_name = args.get(i + 1).unwrap_or(&"cm-c").to_string();
                i += 2;
            }
            "--slots" => {
                slots = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1);
                i += 2;
            }
            "--can" => {
                if let Some(cap) = args.get(i + 1) {
                    capabilities.push((*cap).to_string());
                }
                i += 2;
            }
            other => bail!("unrecognised argument: {other}"),
        }
    }

    let home = home()?;
    sirji::Settings::load(&home)?.activate();
    let config = load_config(&home)?;
    let secret = keys(&home).secret(&id52::decode(&config.key)?)?;

    // We listen, because a caller granted this machine dials it directly.
    let endpoint = sirji::bind(secret).await?;
    println!(
        "worker `{}` listening as {} — {} slot(s), can {:?}",
        config.name, config.key, slots, capabilities
    );

    let assigned: Assigned = Arc::new(Mutex::new(Default::default()));

    tokio::spawn({
        let config = config.clone();
        let home = home.clone();
        let hints = listening(&endpoint);
        let assigned = assigned.clone();
        async move {
            loop {
                if let Err(e) = offer(
                    &config,
                    &home,
                    &controller_name,
                    slots,
                    &capabilities,
                    &hints,
                    &assigned,
                )
                .await
                {
                    eprintln!("controller unreachable: {e:#}");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    });

    let name = config.name.clone();
    while let Some(incoming) = endpoint.accept().await {
        let assigned = assigned.clone();
        let name = name.clone();
        tokio::spawn(async move {
            if let Err(e) = do_work(incoming, assigned, name).await
                && !quiet(&e)
            {
                eprintln!("job failed: {e:#}");
            }
        });
    }
    Ok(())
}

/// Find the controller through our own parent, register, and hold the connection.
///
/// The controller is a **sibling** — another device of the same sirji. We hold no
/// `network.toml` and cannot look it up ourselves, and hardcoding its address would
/// go stale the next time it restarts on a new port.
async fn offer(
    config: &Config,
    home: &std::path::Path,
    controller_name: &str,
    slots: u32,
    capabilities: &[String],
    hints: &[String],
    assigned: &Assigned,
) -> Result<()> {
    let secret = keys(home).secret(&id52::decode(&config.key)?)?;
    let endpoint = sirji::endpoint::bind_dialer(secret).await?;

    let found = sirji::daemon::ask_as_device(
        &endpoint,
        &config.parent,
        &config.parent_hints,
        &sirji::proto::Ask::ResolveLocal {
            name: controller_name.to_string(),
        },
    )
    .await?;

    let (key, controller_hints) = match found {
        sirji::proto::Say::Resolved { device, hints, .. } => (device, hints),
        sirji::proto::Say::No { reason } => bail!("cannot find `{controller_name}`: {reason}"),
    };

    let conn = dial_any(&endpoint, id52::decode(&key)?, &controller_hints).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut recv = BufReader::new(recv);

    write_line(
        &mut send,
        &Register {
            name: config.name.clone(),
            slots,
            capabilities: capabilities.to_vec(),
            hints: hints.to_vec(),
        },
    )
    .await?;
    println!("offered to `{controller_name}`");

    // Orders arrive here for as long as we are registered. `send` stays in scope
    // deliberately: dropping it closes the stream, which the controller would read
    // as us having left.
    while let Some(order) = read_line::<Aadesh>(&mut recv).await? {
        match order {
            Aadesh::Assigned { reservation, caller, limits } => {
                println!("assigned to {caller} as {reservation}");
                assigned.lock().await.insert(reservation, (caller, limits));
            }
            Aadesh::Freed { reservation } => {
                println!("released from {reservation}");
                assigned.lock().await.remove(&reservation);
            }
        }
    }
    drop(send);
    endpoint.close().await;
    Ok(())
}

/// Run what a caller sends, if it is genuinely ours to run.
async fn do_work(incoming: sirji::Incoming, assigned: Assigned, me: String) -> Result<()> {
    let conn = incoming.await?;
    let caller = id52::encode(&conn.remote_id());

    let (mut send, recv) = conn.accept_bi().await?;
    let mut recv = BufReader::new(recv);

    let Some(Upadesh::Run { reservation, command, index, total }) =
        read_line::<Upadesh>(&mut recv).await?
    else {
        return Ok(());
    };

    // Two checks, both necessary. The reservation must be one the controller told
    // us about, and the caller must be who it was assigned to. No ticket is
    // consulted: the controller already decided, and re-deciding at the edge is
    // exactly how a worker ends up needing a policy file of its own.
    let holder = assigned.lock().await.get(&reservation).cloned();
    let refusal = match holder {
        None => Some(format!("we hold no reservation {reservation}")),
        Some((expected, _)) if expected != caller => Some(format!("{reservation} is not yours")),
        Some(_) => None,
    };
    if let Some(reason) = refusal {
        println!("refused {caller}: {reason}");
        write_line(&mut send, &Outcome::No { reason }).await?;
        send.finish()?;
        conn.closed().await;
        return Ok(());
    }

    println!("running shard {}/{total} for {reservation}: {command:?}", index + 1);
    // Placeholder for the real runner. Enough to prove the work reached the right
    // machine under the right reservation, which is what had to be demonstrated.
    let output = format!("{me} ran shard {}/{total} of {command:?}", index + 1);
    write_line(&mut send, &Outcome::Done { worker: me, index, output }).await?;
    send.finish()?;
    conn.closed().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// the tester
// ---------------------------------------------------------------------------

async fn test(target: &str, why: &str, args: &[&str]) -> Result<()> {
    let mut nivedana = Nivedana {
        why: why.to_string(),
        role: std::env::var("CM_T_ROLE").ok(),
        ..Default::default()
    };
    let mut command = "echo hello".to_string();
    let mut abandon = false;

    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--abandon" => {
                // Walk away holding the grant, to show the controller taking it
                // back on its own. Nothing a real caller would ask for; everything
                // a crashed one does by accident.
                abandon = true;
                i += 1;
            }
            "--count" => {
                nivedana.count = args.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "--need" => {
                if let Some(cap) = args.get(i + 1) {
                    nivedana.capabilities.push((*cap).to_string());
                }
                i += 2;
            }
            "--role" => {
                nivedana.role = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            "--run" => {
                if let Some(cmd) = args.get(i + 1) {
                    command = (*cmd).to_string();
                }
                i += 2;
            }
            other => bail!("unrecognised argument: {other}"),
        }
    }

    let home = home()?;
    sirji::Settings::load(&home)?.activate();
    let config = load_config(&home)?;
    let secret = keys(&home).secret(&id52::decode(&config.key)?)?;

    let (name, org) = target
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("{target:?} is not name@org"))?;

    // Our parent knows who the organisation is. We do not, and should not.
    let endpoint = sirji::endpoint::bind_dialer(secret).await?;
    let resolved = sirji::daemon::ask_as_device(
        &endpoint,
        &config.parent,
        &config.parent_hints,
        &sirji::proto::Ask::ResolveFor {
            name: name.to_string(),
            alias: org.to_string(),
        },
    )
    .await?;
    let (device, ticket, hints) = match resolved {
        sirji::proto::Say::Resolved { device, ticket, hints } => (device, ticket, hints),
        sirji::proto::Say::No { reason } => bail!("{reason}"),
    };
    eprintln!("resolved {target} -> {device}");

    let conn = dial_any(&endpoint, id52::decode(&device)?, &hints).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut recv = BufReader::new(recv);

    // Everything from here has to unwind the same way, whatever the answer was:
    // a counter or a refusal that skipped the close left iroh's tasks to be
    // cancelled out from under the runtime, which surfaced as a panic on exit.
    let outcome = async {
        write_line(&mut send, &Knock { ticket: Some(ticket) }).await?;
        write_line(&mut send, &Plea::Nivedana(nivedana)).await?;

        let verdict: Verdict = read_line(&mut recv)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the controller said nothing"))?;

        let (reservation, workers, expires_in) = match verdict {
            Verdict::Grant { reservation, workers, expires_in, rationale } => {
                println!("granted {} machine(s) as {reservation}", workers.len());
                if let Some(r) = rationale {
                    println!("  ({r})");
                }
                (reservation, workers, expires_in)
            }
            Verdict::Counter { count, rationale } => {
                println!("countered: {count} — {rationale}");
                return Ok(());
            }
            Verdict::Deny { rationale } => bail!("denied: {rationale}"),
            Verdict::Ok => bail!("unexpected acknowledgement"),
        };
        println!("  expires in {expires_in}s unless released");
        for w in &workers {
            println!("  {} ({})", w.name, w.key);
        }

        // Talk to each machine directly. The controller allocated; it is not a proxy.
        let total = workers.len() as u32;
        for (index, handle) in workers.iter().enumerate() {
            match run_on(&endpoint, handle, &reservation, &command, index as u32, total).await {
                Ok(Outcome::Done { worker, output, .. }) => println!("  {worker}: {output}"),
                Ok(Outcome::No { reason }) => println!("  {}: refused — {reason}", handle.name),
                Err(e) => eprintln!("  {}: {e:#}", handle.name),
            }
        }

        if abandon {
            println!("walking away still holding {reservation}");
            return Ok(());
        }

        // Give them back at once. A duration hint sizes a plan; it never justifies
        // holding capacity idle.
        write_line(&mut send, &Plea::Release { reservation: reservation.clone() }).await?;
        match read_line::<Verdict>(&mut recv).await? {
            Some(Verdict::Ok) => println!("released {reservation}"),
            Some(Verdict::Deny { rationale }) => eprintln!("release refused: {rationale}"),
            _ => eprintln!("release unacknowledged"),
        }
        Ok(())
    }
    .await;

    let _ = send.finish();
    conn.close(0u32.into(), b"done");
    endpoint.close().await;
    outcome
}

async fn run_on(
    endpoint: &sirji::Endpoint,
    handle: &WorkerHandle,
    reservation: &str,
    command: &str,
    index: u32,
    total: u32,
) -> Result<Outcome> {
    let conn = dial_any(endpoint, id52::decode(&handle.key)?, &handle.hints).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut recv = BufReader::new(recv);

    write_line(
        &mut send,
        &Upadesh::Run {
            reservation: reservation.to_string(),
            command: command.to_string(),
            index,
            total,
        },
    )
    .await?;
    send.finish()?;

    let outcome = read_line(&mut recv)
        .await?
        .ok_or_else(|| anyhow::anyhow!("{} said nothing", handle.name))?;
    conn.close(0u32.into(), b"done");
    Ok(outcome)
}

/// Hints first, discovery second. The hints came from the machine itself moments
/// ago, by way of its controller.
async fn dial_any(
    endpoint: &sirji::Endpoint,
    target: sirji::PublicKey,
    hints: &[String],
) -> Result<sirji::Connection> {
    match sirji::endpoint::dial_hints(endpoint, target, hints).await {
        Ok(conn) => Ok(conn),
        Err(_) => sirji::dial(endpoint, target).await,
    }
}

/// The first line a tester sends: the ticket its own sirji issued.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Knock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ticket: Option<sirji::Ticket>,
}
