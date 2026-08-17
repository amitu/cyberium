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

use anyhow::{Context, Result, bail};
use fleet::{Fleet, Shortfall, Worker};
use policy::Ruling;
use proto::{
    Aadesh, Artifact, Limits, Nivedana, Outcome, Plea, Register, Upadesh, Verdict, WorkerHandle,
};
use sirji::id52;
// `write_all` here is quinn's own inherent method on the stream, not the tokio
// trait's — importing `AsyncWriteExt` for it would be importing nothing.
use tokio::io::{AsyncBufReadExt, BufReader};
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
        --repo <url>     fetch this before running   --ref <commit>  which commit
        --dir <subdir>   run below the repo root     --setup <cmd>   run once first
        --cwd <dir>      run here instead, when the machine already has the code
        --env K=V        extra environment           --collect <path>  bring back
        --artifacts <d>  where to put what comes back
        --abandon        keep the grant and walk away, to watch it time out
        In --run, --env and --collect: {shard} is 1-based, {index} 0-based,
        {shards} the total.
        Each shard gets its own checkout, deleted when the reservation ends.

Test runners plug in on top of `cm test` rather than inside cm: a runner that knew
about Playwright would owe the same favour to jest, pytest and whatever comes next.
See plugins/ — the Playwright one is a `npm test` away.

$CM_HOME defaults to ~/.cm. A device has its own home because a device may be on
another machine.";

fn main() -> Result<std::process::ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let ok = std::process::ExitCode::SUCCESS;
    match args.as_slice() {
        [] | ["-h"] | ["--help"] | ["help"] => {
            println!("{USAGE}");
            Ok(ok)
        }
        ["init", rest @ ..] => rt()?.block_on(init(rest)).map(|_| ok),
        ["controller"] => rt()?.block_on(controller()).map(|_| ok),
        ["worker", rest @ ..] => rt()?.block_on(worker(rest)).map(|_| ok),
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
async fn listening(endpoint: &sirji::Endpoint) -> Vec<String> {
    sirji::endpoint::reachable_at(endpoint).await
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

/// How an exit status reads to a person.
fn describe(code: Option<i32>) -> String {
    match code {
        Some(0) => "success".to_string(),
        Some(n) => format!("exit {n}"),
        None => "no exit code (killed)".to_string(),
    }
}

fn b64_encode(bytes: &[u8]) -> String {
    data_encoding::BASE64.encode(bytes)
}

fn b64_decode(text: &str) -> Result<Vec<u8>> {
    Ok(data_encoding::BASE64.decode(text.as_bytes())?)
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

    // Anything under work/ belongs to a reservation from a previous life of this
    // process, and every one of those is over. Clearing it at startup is also the
    // recovery path for a worker that was killed mid-run and never got its Freed.
    let stale = home.join("work");
    if stale.exists() {
        match std::fs::remove_dir_all(&stale) {
            Ok(()) => println!("cleared workspaces from a previous run"),
            Err(e) => eprintln!("could not clear {}: {e}", stale.display()),
        }
    }

    tokio::spawn({
        let config = config.clone();
        let home = home.clone();
        let hints = listening(&endpoint).await;
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
                // The workspace dies with the reservation that paid for it. Tying
                // it to the lifecycle that already exists means there is no second
                // policy about when checkouts get cleaned up — and no machine that
                // fills its disk with the last hundred runs.
                let dir = home.join("work").join(&reservation);
                if dir.exists()
                    && let Err(e) = std::fs::remove_dir_all(&dir)
                {
                    eprintln!("could not remove {}: {e}", dir.display());
                }
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

    let Some(Upadesh::Run {
        reservation,
        command,
        workspace,
        cwd,
        env,
        collect,
        index,
        total,
    }) = read_line::<Upadesh>(&mut recv).await?
    else {
        return Ok(());
    };

    // Two checks, both necessary. The reservation must be one the controller told
    // us about, and the caller must be who it was assigned to. No ticket is
    // consulted: the controller already decided, and re-deciding at the edge is
    // exactly how a worker ends up needing a policy file of its own.
    let holder = assigned.lock().await.get(&reservation).cloned();
    let (refusal, limits) = match holder {
        None => (Some(format!("we hold no reservation {reservation}")), Limits::default()),
        Some((expected, _)) if expected != caller => {
            (Some(format!("{reservation} is not yours")), Limits::default())
        }
        Some((_, limits)) => (None, limits),
    };
    if let Some(reason) = refusal {
        println!("refused {caller}: {reason}");
        write_line(&mut send, &Outcome::No { reason }).await?;
        send.finish()?;
        conn.closed().await;
        return Ok(());
    }

    println!("shard {}/{total} for {reservation}: {command}", index + 1);
    let outcome = run_shard(
        &mut send, &me, &command, workspace, cwd, &env, &collect, index, limits, &reservation,
    )
    .await;
    let outcome = outcome.unwrap_or_else(|e| Outcome::No { reason: format!("{e:#}") });
    if let Outcome::Done { code, .. } = &outcome {
        println!("shard {}/{total} finished: {code:?}", index + 1);
    }
    write_line(&mut send, &outcome).await?;
    send.finish()?;
    conn.closed().await;
    Ok(())
}

/// Where a reservation's working trees live: `$CM_HOME/work/<reservation>/shard-N`.
///
/// Per reservation so the whole lot can be deleted when it ends, and per shard
/// underneath because two shards of one run must not share a tree — a `.last-run`
/// file, a lockfile, a build cache written by one and read by the other is exactly
/// how a suite starts passing for reasons nobody chose.
fn work_dir(home: &std::path::Path, reservation: &str, index: u32) -> std::path::PathBuf {
    home.join("work").join(reservation).join(format!("shard-{}", index + 1))
}

/// Fetch the code, then run in it.
///
/// Splitting this from `run_command` keeps one fact visible: getting the workspace
/// is *work the caller is paying for*, so its output is streamed and its failures
/// are reported the same way the command's are. A clone that silently took four
/// minutes would otherwise look like a slow test suite.
#[allow(clippy::too_many_arguments)]
async fn run_shard(
    send: &mut sirji::SendStream,
    me: &str,
    command: &str,
    workspace: Option<proto::Workspace>,
    cwd: Option<String>,
    env: &[(String, String)],
    collect: &[String],
    index: u32,
    limits: Limits,
    reservation: &str,
) -> Result<Outcome> {
    let Some(workspace) = workspace else {
        return run_command(send, me, command, cwd, env, collect, index, limits).await;
    };
    if cwd.is_some() {
        bail!("asked for both a workspace and a working directory — which one?");
    }

    let home = home()?;
    let root = work_dir(&home, reservation, index);
    // A tree from a previous attempt is not a head start, it is contamination.
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;

    if let Some(problem) = fetch(send, &workspace, &root, index, limits).await? {
        return Ok(problem);
    }

    // The suite may live below the repository root. Resolve through the real path so
    // a `dir` of `../../..` cannot walk out of the workspace we just made.
    let mut here = root.clone();
    if let Some(dir) = &workspace.dir {
        here = root.join(dir);
        let (real_root, real_here) = (root.canonicalize()?, here.canonicalize()?);
        if !real_here.starts_with(&real_root) {
            bail!("{dir} is outside the workspace");
        }
    }

    if let Some(setup) = &workspace.setup {
        log(send, index, format!("$ {setup}"), false).await?;
        let outcome = run_command(
            send,
            me,
            setup,
            Some(here.to_string_lossy().into_owned()),
            env,
            &[],
            index,
            limits,
        )
        .await?;
        // Setup failing is not the suite failing, and saying so saves an hour of
        // reading test output for a problem that was `npm ci`.
        match &outcome {
            Outcome::Done { code: Some(0), .. } => {}
            Outcome::Done { code, .. } => {
                return Ok(Outcome::No {
                    reason: format!("setup {:?} failed: {}", setup, describe(*code)),
                });
            }
            _ => return Ok(outcome),
        }
    }

    run_command(
        send,
        me,
        command,
        Some(here.to_string_lossy().into_owned()),
        env,
        collect,
        index,
        limits,
    )
    .await
}

/// Get `workspace.git_ref` into `root`. Returns `Some` if it could not be done.
///
/// Shallow first: one commit is all a test run needs, and on a repository of any
/// size the difference between that and the full history is minutes per machine per
/// run. Falls back to a complete clone, because shallow fetch of a bare commit needs
/// a server that allows it and not every host does.
async fn fetch(
    send: &mut sirji::SendStream,
    workspace: &proto::Workspace,
    root: &std::path::Path,
    index: u32,
    limits: Limits,
) -> Result<Option<Outcome>> {
    let repo = &workspace.repo;
    let git_ref = &workspace.git_ref;
    log(send, index, format!("fetching {git_ref} from {repo}"), false).await?;

    let shallow = format!(
        "git init -q . && git remote add origin {repo} \
         && git fetch -q --depth 1 origin {git_ref} && git checkout -q FETCH_HEAD"
    );
    if git(send, &shallow, root, index, limits).await? {
        return Ok(None);
    }

    log(
        send,
        index,
        "shallow fetch refused — cloning in full".to_string(),
        true,
    )
    .await?;
    // Start over: a half-initialised directory is not a base to build on.
    std::fs::remove_dir_all(root)?;
    std::fs::create_dir_all(root)?;

    let full = format!("git clone -q {repo} . && git checkout -q {git_ref}");
    if git(send, &full, root, index, limits).await? {
        return Ok(None);
    }

    Ok(Some(Outcome::No {
        reason: format!(
            "could not get {git_ref} from {repo} — is it pushed, and can this machine read it?"
        ),
    }))
}

/// Run one git incantation, streaming it. `true` if it worked.
async fn git(
    send: &mut sirji::SendStream,
    script: &str,
    root: &std::path::Path,
    index: u32,
    limits: Limits,
) -> Result<bool> {
    let outcome = run_command(
        send,
        "git",
        script,
        Some(root.to_string_lossy().into_owned()),
        &[],
        &[],
        index,
        limits,
    )
    .await?;
    Ok(matches!(outcome, Outcome::Done { code: Some(0), .. }))
}

async fn log(
    send: &mut sirji::SendStream,
    index: u32,
    line: String,
    stderr: bool,
) -> Result<()> {
    write_line(send, &Outcome::Log { index, line, stderr }).await
}

/// Run the command, streaming its output back as it appears.
///
/// A shell runs it, because a caller should be able to send what they would have
/// typed — pipes, `&&`, a shell function — rather than an argv this code would have
/// to invent a quoting convention for.
#[allow(clippy::too_many_arguments)]
async fn run_command(
    send: &mut sirji::SendStream,
    me: &str,
    command: &str,
    cwd: Option<String>,
    env: &[(String, String)],
    collect: &[String],
    index: u32,
    limits: Limits,
) -> Result<Outcome> {
    let root = match cwd {
        Some(dir) => {
            let path = std::path::PathBuf::from(&dir);
            // Refuse rather than fall back to our own directory. Running the right
            // command in the wrong place produces a plausible, wrong answer.
            if !path.is_dir() {
                bail!("{dir} is not a directory on this machine");
            }
            path
        }
        None => std::env::current_dir()?,
    };

    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&root)
        .envs(env.iter().map(|(k, v)| (k.clone(), v.clone())))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {command:?}"))?;

    let mut out = BufReader::new(child.stdout.take().expect("piped")).lines();
    let mut err = BufReader::new(child.stderr.take().expect("piped")).lines();
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(limits.max_seconds.max(1)));
    tokio::pin!(deadline);

    let code = loop {
        tokio::select! {
            line = out.next_line() => match line? {
                Some(line) => write_line(send, &Outcome::Log { index, line, stderr: false }).await?,
                None => {}
            },
            line = err.next_line() => match line? {
                Some(line) => write_line(send, &Outcome::Log { index, line, stderr: true }).await?,
                None => {}
            },
            status = child.wait() => break status?.code(),
            _ = &mut deadline => {
                // The limit came from the controller with the reservation. Killing
                // is the whole point of having one: a run that ignores its ceiling
                // holds a machine nobody can get back.
                let _ = child.kill().await;
                return Ok(Outcome::No {
                    reason: format!("killed after {}s", limits.max_seconds),
                });
            }
        }
    };

    // Drain whatever was still buffered when the process exited, or the last and
    // most interesting lines — the failure summary — are the ones that go missing.
    while let Some(line) = out.next_line().await? {
        write_line(send, &Outcome::Log { index, line, stderr: false }).await?;
    }
    while let Some(line) = err.next_line().await? {
        write_line(send, &Outcome::Log { index, line, stderr: true }).await?;
    }

    let mut artifacts = Vec::new();
    for path in collect {
        let full = root.join(path);
        match std::fs::read(&full) {
            Ok(bytes) => artifacts.push(Artifact {
                path: path.clone(),
                base64: b64_encode(&bytes),
            }),
            // Not an error: a shard that failed early may legitimately have
            // produced no report, and the exit code already says so.
            Err(e) => eprintln!("no {}: {e}", full.display()),
        }
    }

    Ok(Outcome::Done {
        worker: me.to_string(),
        index,
        code,
        artifacts,
    })
}

// ---------------------------------------------------------------------------
// the tester
// ---------------------------------------------------------------------------

async fn test(target: &str, why: &str, args: &[&str]) -> Result<std::process::ExitCode> {
    let mut nivedana = Nivedana {
        why: why.to_string(),
        role: std::env::var("CM_T_ROLE").ok(),
        ..Default::default()
    };
    let mut job = Job {
        command: "echo hello".to_string(),
        workspace: None,
        cwd: None,
        env: Vec::new(),
        collect: Vec::new(),
    };
    let mut artifacts_dir = "cm-artifacts".to_string();
    let mut abandon = false;
    let (mut repo, mut git_ref, mut dir, mut setup) = (None, None, None, None);

    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--repo" => {
                repo = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            "--ref" => {
                git_ref = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            "--dir" => {
                dir = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            "--setup" => {
                setup = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            "--cwd" => {
                job.cwd = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            "--env" => {
                if let Some((k, v)) = args.get(i + 1).and_then(|kv| kv.split_once('=')) {
                    job.env.push((k.to_string(), v.to_string()));
                }
                i += 2;
            }
            "--collect" => {
                if let Some(path) = args.get(i + 1) {
                    job.collect.push((*path).to_string());
                }
                i += 2;
            }
            "--artifacts" => {
                artifacts_dir = args.get(i + 1).unwrap_or(&"cm-artifacts").to_string();
                i += 2;
            }
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
                    job.command = (*cmd).to_string();
                }
                i += 2;
            }
            other => bail!("unrecognised argument: {other}"),
        }
    }

    if let Some(repo) = repo {
        // A ref is not optional. Defaulting to the default branch would mean two
        // shards of one run could test two different commits, and the report would
        // not say so.
        let git_ref = git_ref.ok_or_else(|| anyhow::anyhow!("--repo also needs --ref"))?;
        job.workspace = Some(proto::Workspace { repo, git_ref, dir, setup });
    } else if git_ref.is_some() || dir.is_some() || setup.is_some() {
        bail!("--ref, --dir and --setup only mean something with --repo");
    }

    let session = Session {
        target: target.to_string(),
        nivedana,
        job,
        artifacts_dir,
        abandon,
    };
    let results = run(session).await?;
    Ok(exit_with(&results))
}

/// One complete errand: ask, use what you are given, give it back.
struct Session {
    target: String,
    nivedana: Nivedana,
    job: Job,
    artifacts_dir: String,
    abandon: bool,
}

/// What a session came back with. Empty when the plea was countered — the caller
/// was told why, and nothing ran.
type Results = Vec<Result<Shard>>;

/// Resolve, plead, dispatch, release.
///
/// The whole of what cm offers a caller. Everything a test runner needs on top of
/// this — shard arithmetic, report formats, merging — belongs to that runner's
/// plugin, not here.
async fn run(session: Session) -> Result<Results> {
    let home = home()?;
    sirji::Settings::load(&home)?.activate();
    let config = load_config(&home)?;
    let secret = keys(&home).secret(&id52::decode(&config.key)?)?;

    let (name, org) = session
        .target
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("{:?} is not name@org", session.target))?;

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
    eprintln!("resolved {} -> {device}", session.target);

    let conn = dial_any(&endpoint, id52::decode(&device)?, &hints).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut recv = BufReader::new(recv);

    // Everything from here has to unwind the same way, whatever the answer was:
    // a counter or a refusal that skipped the close left iroh's tasks to be
    // cancelled out from under the runtime, which surfaced as a panic on exit.
    let outcome = async {
        write_line(&mut send, &Knock { ticket: Some(ticket) }).await?;
        write_line(&mut send, &Plea::Nivedana(session.nivedana)).await?;

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
                return Ok(Vec::new());
            }
            Verdict::Deny { rationale } => bail!("denied: {rationale}"),
            Verdict::Ok => bail!("unexpected acknowledgement"),
        };
        println!("  expires in {expires_in}s unless released");
        for w in &workers {
            println!("  {} ({})", w.name, w.key);
        }

        // Talk to each machine directly. The controller allocated; it is not a proxy.
        let results = dispatch(&endpoint, &workers, &reservation, &session.job).await;
        let done: Vec<&Shard> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
        for result in &results {
            match result {
                Ok(shard) => println!(
                    "  {} finished shard {} with {}",
                    shard.worker,
                    shard.index + 1,
                    describe(shard.code)
                ),
                Err(e) => eprintln!("  {e:#}"),
            }
        }
        if done.iter().any(|s| !s.artifacts.is_empty()) {
            let dir = std::path::PathBuf::from(&session.artifacts_dir);
            for path in save_artifacts(&dir, &done)? {
                println!("  kept {}", path.display());
            }
        }

        if session.abandon {
            println!("walking away still holding {reservation}");
            return Ok(results);
        }

        // Give them back at once — even when shards failed. A failed run is exactly
        // when capacity is most worth returning, and holding it hostage to an error
        // path is how a fleet fills up with machines nobody is using.
        write_line(&mut send, &Plea::Release { reservation: reservation.clone() }).await?;
        match read_line::<Verdict>(&mut recv).await? {
            Some(Verdict::Ok) => println!("released {reservation}"),
            Some(Verdict::Deny { rationale }) => eprintln!("release refused: {rationale}"),
            _ => eprintln!("release unacknowledged"),
        }
        Ok(results)
    }
    .await;

    let _ = send.finish();
    conn.close(0u32.into(), b"done");
    endpoint.close().await;
    outcome
}

/// Exit as the run did. A distributed run that swallows a failing shard is worse
/// than no run at all: CI goes green on a suite that did not pass.
fn exit_with(results: &Results) -> std::process::ExitCode {
    let failed = results.iter().any(|r| match r {
        Ok(shard) => shard.code != Some(0),
        Err(_) => true,
    });
    if failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }
}

// ---------------------------------------------------------------------------
// dispatch
// ---------------------------------------------------------------------------

/// One command, to be run once per granted machine with its shard number filled in.
#[derive(Debug, Clone)]
struct Job {
    /// May contain `{shard}` (1-based), `{index}` (0-based) and `{shards}`.
    command: String,
    /// Code to fetch first. The normal case for a real fleet: a worker starts with
    /// nothing, and expecting it to already have your repo means expecting somebody
    /// to have prepared that machine by hand.
    workspace: Option<proto::Workspace>,
    cwd: Option<String>,
    env: Vec<(String, String)>,
    collect: Vec<String>,
}

impl Job {
    /// Fill in this machine's place in the plan.
    ///
    /// 1-based `{shard}` because every runner that takes a shard argument counts
    /// from one; 0-based `{index}` as well, because filenames usually do not.
    fn shaped(&self, text: &str, index: u32, total: u32) -> String {
        text.replace("{shard}", &(index + 1).to_string())
            .replace("{index}", &index.to_string())
            .replace("{shards}", &total.to_string())
    }
}

/// What one machine did.
struct Shard {
    worker: String,
    index: u32,
    code: Option<i32>,
    artifacts: Vec<Artifact>,
}

/// Run the job on every granted machine at once.
///
/// At once, not in turn: running N shards sequentially would take exactly as long
/// as running them here, which is the entire thing anyone is paying for. Each
/// machine gets its own connection, and its output is printed as it arrives with
/// the machine's name in front — interleaved on purpose, because a stalled shard
/// should be visible while it is stalling rather than after everything else ends.
async fn dispatch(
    endpoint: &sirji::Endpoint,
    workers: &[WorkerHandle],
    reservation: &str,
    job: &Job,
) -> Vec<Result<Shard>> {
    let total = workers.len() as u32;
    let mut running = Vec::new();

    for (index, handle) in workers.iter().enumerate() {
        let endpoint = endpoint.clone();
        let handle = handle.clone();
        let reservation = reservation.to_string();
        let job = job.clone();
        running.push(tokio::spawn(async move {
            run_on(&endpoint, &handle, &reservation, &job, index as u32, total).await
        }));
    }

    let mut out = Vec::new();
    for task in running {
        out.push(match task.await {
            Ok(result) => result,
            Err(e) => Err(anyhow::anyhow!("shard task failed: {e}")),
        });
    }
    out
}

async fn run_on(
    endpoint: &sirji::Endpoint,
    handle: &WorkerHandle,
    reservation: &str,
    job: &Job,
    index: u32,
    total: u32,
) -> Result<Shard> {
    let conn = dial_any(endpoint, id52::decode(&handle.key)?, &handle.hints).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut recv = BufReader::new(recv);

    write_line(
        &mut send,
        &Upadesh::Run {
            reservation: reservation.to_string(),
            command: job.shaped(&job.command, index, total),
            workspace: job.workspace.clone(),
            cwd: job.cwd.clone(),
            env: job
                .env
                .iter()
                .map(|(k, v)| (k.clone(), job.shaped(v, index, total)))
                .collect(),
            collect: job
                .collect
                .iter()
                .map(|p| job.shaped(p, index, total))
                .collect(),
            index,
            total,
        },
    )
    .await?;
    send.finish()?;

    // Logs until the one terminal message. The worker sends them as they happen,
    // so this loop is also what keeps the connection demonstrably alive.
    loop {
        let Some(outcome) = read_line::<Outcome>(&mut recv).await? else {
            bail!("{} stopped talking mid-run", handle.name);
        };
        match outcome {
            Outcome::Log { line, stderr, .. } => {
                if stderr {
                    eprintln!("[{}] {line}", handle.name);
                } else {
                    println!("[{}] {line}", handle.name);
                }
            }
            Outcome::Done { worker, index, code, artifacts } => {
                conn.close(0u32.into(), b"done");
                return Ok(Shard { worker, index, code, artifacts });
            }
            Outcome::No { reason } => {
                conn.close(0u32.into(), b"done");
                bail!("{} refused: {reason}", handle.name);
            }
        }
    }
}

/// Write what the machines sent back, under one directory per shard so two shards
/// producing the same filename do not overwrite each other.
fn save_artifacts(dir: &std::path::Path, shards: &[&Shard]) -> Result<Vec<std::path::PathBuf>> {
    let mut written = Vec::new();
    for shard in shards {
        for artifact in &shard.artifacts {
            // The path came from another machine. Take the filename and nothing
            // else: a worker that answered with `../../etc/something` should not be
            // able to decide where our files land.
            let name = std::path::Path::new(&artifact.path)
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("{:?} has no filename", artifact.path))?;
            let into = dir.join(format!("shard-{}", shard.index + 1));
            std::fs::create_dir_all(&into)?;
            let path = into.join(name);
            std::fs::write(&path, b64_decode(&artifact.base64)?)
                .with_context(|| format!("writing {}", path.display()))?;
            written.push(path);
        }
    }
    Ok(written)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn job(command: &str) -> Job {
        Job {
            command: command.into(),
            workspace: None,
            cwd: None,
            env: vec![],
            collect: vec![],
        }
    }

    #[test]
    fn shard_numbering_matches_what_runners_expect() {
        let j = job("");
        // 1-based for the runner, 0-based for filenames, and both available at
        // once — a test runner counts shards from one, a directory listing does not.
        assert_eq!(j.shaped("--shard={shard}/{shards}", 0, 3), "--shard=1/3");
        assert_eq!(j.shaped("--shard={shard}/{shards}", 2, 3), "--shard=3/3");
        assert_eq!(j.shaped("report-{index}.zip", 0, 3), "report-0.zip");
    }

    #[test]
    fn substitution_reaches_env_and_collected_paths_too() {
        // The blob filename is set by env on the worker and read back by path
        // here; if only the command were substituted the two would disagree.
        let j = Job {
            command: "run".into(),
            workspace: None,
            cwd: None,
            env: vec![("OUT".into(), "blob-{shard}.zip".into())],
            collect: vec!["blob-{shard}.zip".into()],
        };
        assert_eq!(j.shaped(&j.env[0].1, 1, 2), "blob-2.zip");
        assert_eq!(j.shaped(&j.collect[0], 1, 2), "blob-2.zip");
    }

    #[test]
    fn a_worker_cannot_choose_where_our_files_land() {
        // The path in an artifact came from another machine. Only its filename is
        // ours to trust: answering with `../../..` must not write outside the
        // directory the caller named.
        let dir = std::env::temp_dir().join(format!("cm-test-{}", std::process::id()));
        let shard = Shard {
            worker: "w".into(),
            index: 0,
            code: Some(0),
            artifacts: vec![Artifact {
                path: "../../../etc/escaped.txt".into(),
                base64: b64_encode(b"nope"),
            }],
        };
        let written = save_artifacts(&dir, &[&shard]).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0], dir.join("shard-1").join("escaped.txt"));
        assert!(written[0].starts_with(&dir), "{:?} escaped", written[0]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn shards_are_kept_apart() {
        // Two machines running the same command produce the same filename. One
        // directory each, or the second silently overwrites the first's results.
        let dir = std::env::temp_dir().join(format!("cm-test-two-{}", std::process::id()));
        let shards: Vec<Shard> = (0..2)
            .map(|index| Shard {
                worker: format!("w{index}"),
                index,
                code: Some(0),
                artifacts: vec![Artifact {
                    path: "report.zip".into(),
                    base64: b64_encode(format!("shard {index}").as_bytes()),
                }],
            })
            .collect();
        let written = save_artifacts(&dir, &shards.iter().collect::<Vec<_>>()).unwrap();
        assert_eq!(written.len(), 2);
        assert_ne!(written[0], written[1]);
        assert_eq!(std::fs::read(&written[1]).unwrap(), b"shard 1");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn every_shard_gets_its_own_tree() {
        let home = std::path::Path::new("/cm");
        let one = work_dir(home, "r7", 0);
        let two = work_dir(home, "r7", 1);
        // Two shards of one run must not share a checkout: a lockfile or a build
        // cache written by one and read by the other is how a suite starts passing
        // for reasons nobody chose.
        assert_ne!(one, two);
        // And both sit under the reservation, so ending it removes the lot.
        let reservation = home.join("work").join("r7");
        assert!(one.starts_with(&reservation) && two.starts_with(&reservation));
    }

    #[test]
    fn a_failing_shard_fails_the_run() {
        let ok = |code| Ok(Shard { worker: "w".into(), index: 0, code, artifacts: vec![] });
        let red = std::process::ExitCode::FAILURE;
        let green = std::process::ExitCode::SUCCESS;
        // No way to compare ExitCode, so compare the debug form — enough to catch
        // the inversion, which is the mistake that matters here.
        assert_eq!(format!("{:?}", exit_with(&vec![ok(Some(0))])), format!("{green:?}"));
        assert_eq!(format!("{:?}", exit_with(&vec![ok(Some(0)), ok(Some(1))])), format!("{red:?}"));
        // Killed for exceeding its limit is not success either.
        assert_eq!(format!("{:?}", exit_with(&vec![ok(None)])), format!("{red:?}"));
        // Neither is a shard we never heard back from.
        assert_eq!(
            format!("{:?}", exit_with(&vec![Err(anyhow::anyhow!("gone"))])),
            format!("{red:?}")
        );
    }


}
