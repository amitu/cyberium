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


use std::sync::Arc;

use anyhow::{Context, Result, bail};
use cyberium::proto::{
    Aadesh, Answer, Artifact, Knock, Limits, Nivedana, Outcome, Plea, Register, Upadesh,
    Verdict, WorkerHandle,
};
use cyberium::{
    Config, Published, WELL_KNOWN, admin, b64_decode, b64_encode, config_path, controller, describe, home, keys,
    listening, load_config, policytest, proto, quiet, read_line, tenant, upload, write_line,
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

  cm whoami
        this device's name and key — what `cm admin add` wants

  cm admin add <name> <id52> [--note <text>]
  cm admin list
        who may look at and change how this controller runs. Run on the
        controller itself: membership is a list the host writes by hand, never
        something a device acquires by connecting. Being one of our own devices
        is not enough — a worker is one of those.

  cm admin fleet [--controller <name>]
  cm admin reservations [--controller <name>]
  cm admin spend [--controller <name>]
        look inside a running controller, as an admin device.

  cm tenant add <name> [--ceiling N] [--credits N] [--window SECS]
                       [--member <alias>]... [--admin <alias>]... [--note <text>]
  cm tenant list
        onboard whoever this controller serves — always, self-hosted too, where
        a tenant is usually a team. Members are the caller aliases our own sirji
        knows them by; with none given, the tenant's own name is its only member.
        They write policy.md; you write tenant.toml, which is what stops a
        tenant setting its own quota or claiming somebody else's callers.
        --admin also makes them a member, and is the only way anybody may change
        that tenant's rules. Absent means nobody: authority over a rule cannot
        come from the rule. Also in tenant.toml: [facts], which this deployment
        attests about them — a plan, a team, an entitlement — for their policy to
        read and cm never to interpret.

  cm worker [--controller <name>] [--can <cap>]... [--rate N]
        offer this machine to the controller. One tenancy at a time — for
        concurrency run more of these, and let the OS provide the limits and
        isolation. --rate is what it costs in credits per minute while held;
        the machine announces its own, because the machine is what knows.
        Unstated means 1, never free.
        --pre <cmd>   make the machine fit before each tenancy
        --post <cmd>  take back what the last tenant left
        Both belong to whoever runs the machine: a caller cannot supply them,
        skip them, or see their output. A --post that fails takes the machine
        out of the fleet rather than lending out a dirty one.

  cm test <host-or-id52> [...]
        the same, from a machine with nothing enrolled — a CI runner. Mints a key
        for this run, gets a token whose audience *is* that key, and throws the key
        away. No shared secret, and a token scraped from a build log names an
        audience nobody holds. The host says whose tokens count, in issuers.toml.
        On GitHub Actions give the job `permissions: id-token: write`; elsewhere
        set CM_ATTEST_CMD to a command that prints a token for {audience}.
        A host name is better than a key in a variable: it asks
        https://<host>/.well-known/cm-controller which controller to use, so a
        key rotation changes nothing. The controller prints that document.

  cm test <name@org> --ping
        check the whole chain — identity, our sirji, resolution, dial, ticket —
        and say which link is broken. Takes no machine from anybody.

  cm test <name@org> \"<why>\" [--count N] [--need <cap>]... [--run <cmd>]
        ask for machines, use them, and give them back
        --repo <url>     fetch this before running   --ref <commit>  which commit
        --dir <subdir>   run below the repo root     --setup <cmd>   run once first
        --cwd <dir>      run here instead, when the machine already has the code
        --env K=V        extra environment           --collect <path>  bring back
        --artifacts <d>  where to put what comes back
        --dry-run        what would I get? takes nothing from anybody
        --abandon        keep the grant and walk away, to watch it time out
        In --run, --env and --collect: {shard} is 1-based, {index} 0-based,
        {shards} the total.
        Each shard gets its own checkout, deleted when the reservation ends.
        Any other --key <value> is yours, not cm's: `--plea nightly-regression`,
        `--incident INC-4471`, `--urgent`. cm attaches no meaning to any of them
        and passes them on; what each is worth is written in your own policy.
        CM_SAY='plea=nightly,incident=INC-1' does the same from a CI job.

  cm policy-test [<dir>] [--repeat N] [--only <substring>]
        run the cases in <dir>/policy-tests/ against <dir>'s own rules. No
        controller and no fleet: this belongs in the repository the policy lives
        in, before anything is uploaded. --repeat asks the same question more
        than once, which is a different question: whether prose is written
        clearly enough to hold every time.

  cm upload-policy <name@org> [<dir>]
        replace what a tenant has written down. Only for an admin named in
        tenant.toml. Replaces rather than merges — the folder *is* the policy, so
        a file left behind is a rule that exists on the controller and in no
        repository. Refused whole if it will not parse, so the policy that works
        stays in force.

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
        ["controller"] => rt()?.block_on(controller::controller()).map(|_| ok),
        ["tenant", "add", alias, rest @ ..] => tenant_add(alias, rest).map(|_| ok),
        ["tenant", "list"] => tenant_list().map(|_| ok),
        ["whoami"] => whoami().map(|_| ok),
        ["admin", "add", name, key, rest @ ..] => admin_add(name, key, rest).map(|_| ok),
        ["admin", "list"] => admin_list().map(|_| ok),
        ["admin", "fleet", rest @ ..] => {
            rt()?.block_on(inspect(proto::Look::Fleet, rest)).map(|_| ok)
        }
        ["admin", "reservations", rest @ ..] => rt()?
            .block_on(inspect(proto::Look::Reservations, rest))
            .map(|_| ok),
        ["admin", "spend", rest @ ..] => {
            rt()?.block_on(inspect(proto::Look::Spend, rest)).map(|_| ok)
        }
        ["worker", rest @ ..] => rt()?.block_on(worker(rest)).map(|_| ok),
        // `why` is positional but optional: a ping asks for nothing, so making
        // somebody justify one would be a small daily absurdity.
        // `policy-test`, not `test-policy`: it is a sister of `cm test`, and reading the
        // pair aloud is how anybody remembers which is which.
        ["policy-test", rest @ ..] => rt()?.block_on(policy_test(rest)),
        ["upload-policy", target, rest @ ..] => rt()?.block_on(upload_policy(
            target,
            std::path::Path::new(rest.first().copied().unwrap_or(".")),
        )),
        ["test", target, rest @ ..] if rest.contains(&"--ping") => rt()?.block_on(ping(target)),
        // The reason is optional: a caller naming one of the organisation's own pleas
        // has nothing of their own to say, and a tenant with a catalogue would refuse
        // the text anyway. A leading `--flag` is an argument, not a reason.
        ["test", target, why, rest @ ..] if !why.starts_with("--") => {
            rt()?.block_on(test(target, Some(why), rest))
        }
        ["test", target, rest @ ..] => rt()?.block_on(test(target, None, rest)),
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
// a worker
// ---------------------------------------------------------------------------

/// Reservations this machine has been told about.
type Assigned = Arc<Mutex<std::collections::BTreeMap<String, Held>>>;

/// One tenancy of this machine.
#[derive(Debug, Clone)]
struct Held {
    caller: String,
    limits: Limits,
    state: State,
}

/// Whether this machine is fit to be used yet.
///
/// A machine is not ready the instant it is assigned: the operator's `--pre` script
/// runs first. Without this the caller — who is told about the grant at the same
/// moment the machine is — would dial straight into a box mid-cleanup.
#[derive(Debug, Clone)]
enum State {
    Preparing,
    Ready,
    /// Preparation failed. The machine stays assigned and refuses work: handing it
    /// over anyway is how one tenant's leftovers end up in another tenant's run.
    Unusable(String),
}

/// What the operator wants run around each tenancy.
///
/// These belong to whoever runs the machine, not to whoever borrows it. A caller
/// cannot supply them, skip them, or see their output — the whole point is what
/// happens *between* tenants, and one tenant should learn nothing about the last.
#[derive(Debug, Clone, Default)]
struct Hygiene {
    /// Before the machine is usable: make it fit for whoever is next.
    pre: Option<String>,
    /// After the reservation ends, released or expired: take back what was left.
    post: Option<String>,
}

/// How long an operator's script may take before the machine is written off.
///
/// Generous, because cleanup can mean deleting a lot. Bounded, because a hygiene
/// script that hangs forever is a machine that never comes back to the fleet — and
/// silently, since nobody is waiting on it.
const HYGIENE_SECS: u64 = 600;

/// How long a caller waits for `--pre` before giving up on this machine.
const PREPARE_PATIENCE_SECS: u64 = HYGIENE_SECS;

async fn worker(args: &[&str]) -> Result<()> {
    let mut controller_name = "cm-c".to_string();
    let mut capabilities: Vec<String> = Vec::new();
    let mut rate = 1u32;
    let mut hygiene = Hygiene::default();

    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--pre" => {
                hygiene.pre = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            "--post" => {
                hygiene.post = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            "--controller" => {
                controller_name = args.get(i + 1).unwrap_or(&"cm-c").to_string();
                i += 2;
            }
            "--rate" => {
                // What this machine costs, in credits per minute while held. The
                // machine announces it because the machine is what knows.
                rate = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1);
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
        "worker `{}` listening as {} — {rate} credit(s)/min, can {:?}",
        config.name, config.key, capabilities
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
                    &capabilities,
                    rate,
                    &hints,
                    &assigned,
                    &hygiene,
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
#[allow(clippy::too_many_arguments)]
async fn offer(
    config: &Config,
    home: &std::path::Path,
    controller_name: &str,
    capabilities: &[String],
    rate: u32,
    hints: &[String],
    assigned: &Assigned,
    hygiene: &Hygiene,
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
            capabilities: capabilities.to_vec(),
            rate,
            hints: hints.to_vec(),
        },
    )
    .await?;
    println!("offered to `{controller_name}`");

    // Set when a tenancy ended and the operator has a cleanup script: we leave the
    // fleet first, then scrub, then come back.
    let mut cleanup_after: Option<String> = None;

    // Orders arrive here for as long as we are registered. `send` stays in scope
    // deliberately: dropping it closes the stream, which the controller would read
    // as us having left.
    while let Some(order) = read_line::<Aadesh>(&mut recv).await? {
        match order {
            Aadesh::Assigned { reservation, caller, limits } => {
                println!("assigned to {caller} as {reservation}");
                assigned.lock().await.insert(
                    reservation.clone(),
                    Held { caller, limits, state: State::Preparing },
                );

                // Prepare in the background: the controller is waiting on this
                // stream for nothing else, and blocking it would stall every other
                // order — including the Freed that ends somebody else's tenancy.
                tokio::spawn({
                    let (assigned, hygiene) = (assigned.clone(), hygiene.clone());
                    let root = config.root.clone();
                    async move {
                        let state = match hygiene.pre {
                            None => State::Ready,
                            Some(script) => {
                                println!("preparing for {reservation}: {script}");
                                match hygiene_run(&script, &root).await {
                                    Ok(()) => State::Ready,
                                    Err(e) => {
                                        eprintln!("cannot prepare for {reservation}: {e:#}");
                                        State::Unusable(format!("{e:#}"))
                                    }
                                }
                            }
                        };
                        if let Some(held) = assigned.lock().await.get_mut(&reservation) {
                            held.state = state;
                        }
                    }
                });
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

                if hygiene.post.is_some() {
                    // Stop offering *before* cleaning, by leaving this loop and
                    // letting the registration close. A machine mid-scrub is not
                    // available, and while it was still registered the controller
                    // could — and did — hand it to somebody new while the last
                    // tenant's cleanup was still running, which defeats the entire
                    // point of having a cleanup step.
                    cleanup_after = Some(reservation);
                    break;
                }
            }
        }
    }

    // Closed first, deliberately: the connection *is* the availability, so dropping
    // it is how this machine says "not me" for as long as the scrubbing takes.
    drop(send);
    endpoint.close().await;

    if let Some(reservation) = cleanup_after {
        let script = hygiene.post.as_deref().expect("only set when there is one");
        println!("left the fleet; cleaning up after {reservation}: {script}");
        if let Err(e) = hygiene_run(script, &config.root).await {
            // A machine whose cleanup failed may still hold the last tenant's
            // source, credentials or state. Being short a machine is much cheaper
            // than lending that one out, so stay gone and exit non-zero — what
            // happens next is for whatever supervises this to decide.
            eprintln!("CLEANUP FAILED after {reservation}: {e:#}");
            eprintln!("staying out of the fleet rather than serving a dirty machine");
            std::process::exit(1);
        }
        println!("clean; offering again");
    }
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

    // Three checks. The reservation must be one the controller told us about, the
    // caller must be who it was assigned to, and this machine must actually be fit
    // to use — the operator's `--pre` may still be running. No ticket is consulted:
    // the controller already decided, and re-deciding at the edge is exactly how a
    // worker ends up needing a policy file of its own.
    let limits = match may_run(&assigned, &reservation, &caller, &mut send, index).await? {
        Ok(limits) => limits,
        Err(reason) => {
            println!("refused {caller}: {reason}");
            write_line(&mut send, &Outcome::No { reason }).await?;
            send.finish()?;
            conn.closed().await;
            return Ok(());
        }
    };

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

/// Decide whether this caller may run here, waiting out `--pre` if it is still going.
///
/// Waiting rather than refusing, because the controller tells the machine and the
/// caller about a grant at the same moment: a caller that dialled promptly would
/// otherwise be turned away for being on time. The wait is visible — a caller
/// staring at silence deserves to know it is the machine being scrubbed and not
/// their suite hanging.
async fn may_run(
    assigned: &Assigned,
    reservation: &str,
    caller: &str,
    send: &mut sirji::SendStream,
    index: u32,
) -> Result<std::result::Result<Limits, String>> {
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(PREPARE_PATIENCE_SECS);
    let mut said = false;

    loop {
        let verdict = {
            let held = assigned.lock().await;
            consider(held.get(reservation), reservation, caller)
        };
        match verdict {
            Admission::Run(limits) => return Ok(Ok(limits)),
            Admission::Refuse(why) => return Ok(Err(why)),
            Admission::Wait => {
                if !said {
                    log(send, index, "waiting for the machine to be prepared".into(), false)
                        .await?;
                    said = true;
                }
                if std::time::Instant::now() >= deadline {
                    return Ok(Err(format!(
                        "this machine was still being prepared after {PREPARE_PATIENCE_SECS}s"
                    )));
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}

/// What to do about this caller, given what the machine knows right now.
#[derive(Debug)]
enum Admission {
    Run(Limits),
    Wait,
    Refuse(String),
}

/// The decision itself, kept apart from the waiting so it can be reasoned about —
/// and tested — without a clock or a connection.
fn consider(held: Option<&Held>, reservation: &str, caller: &str) -> Admission {
    let Some(held) = held else {
        return Admission::Refuse(format!("we hold no reservation {reservation}"));
    };
    if held.caller != caller {
        return Admission::Refuse(format!("{reservation} is not yours"));
    }
    match &held.state {
        State::Ready => Admission::Run(held.limits),
        State::Preparing => Admission::Wait,
        State::Unusable(why) => {
            Admission::Refuse(format!("this machine could not be prepared: {why}"))
        }
    }
}

/// Run one of the operator's hygiene scripts.
///
/// Its output goes to this machine's own log and nowhere else. A caller must not see
/// it: cleanup output is about the *previous* tenant, and the whole reason these
/// scripts exist is that one tenant should learn nothing about the last.
async fn hygiene_run(script: &str, root: &std::path::Path) -> Result<()> {
    let deadline = std::time::Duration::from_secs(HYGIENE_SECS);
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(root)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {script:?}"))?;

    match tokio::time::timeout(deadline, child.wait()).await {
        Ok(status) => {
            let status = status?;
            if !status.success() {
                bail!("{script:?} failed: {status}");
            }
            Ok(())
        }
        Err(_) => {
            let _ = child.kill().await;
            bail!("{script:?} was still running after {HYGIENE_SECS}s")
        }
    }
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
            // `None` means that pipe reached its end, which says nothing about the
            // process: the other pipe may still be talking, and the exit status is
            // what actually ends this loop.
            line = out.next_line() => if let Some(line) = line? {
                write_line(send, &Outcome::Log { index, line, stderr: false }).await?;
            },
            line = err.next_line() => if let Some(line) = line? {
                write_line(send, &Outcome::Log { index, line, stderr: true }).await?;
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

/// `cm policy-test [folder] [--repeat N] [--only substring]`
async fn policy_test(args: &[&str]) -> Result<std::process::ExitCode> {
    let mut dir = ".".to_string();
    let (mut repeat, mut only) = (1u32, None);
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--repeat" => {
                repeat = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(1);
                i += 2;
            }
            "--only" => {
                only = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            other if other.starts_with("--") => bail!("unrecognised argument: {other}"),
            other => {
                dir = other.to_string();
                i += 1;
            }
        }
    }
    policytest::run(std::path::Path::new(&dir), repeat, only.as_deref()).await
}

async fn test(
    target: &str,
    why: Option<&str>,
    args: &[&str],
) -> Result<std::process::ExitCode> {
    let mut nivedana = Nivedana::default();
    // The positional reason, if given, is just another key. cm attaches no meaning to
    // `why` — the tenant's own files decide whether a reason in somebody's own words is
    // worth anything, and from whom.
    if let Some(text) = why.map(str::trim).filter(|w| !w.is_empty()) {
        nivedana.said.insert("why".into(), text.to_string());
    }
    // `CM_SAY` is the same thing for a CI job that cannot easily add arguments:
    // `CM_SAY='plea=nightly-regression,incident=INC-4471'`.
    for pair in std::env::var("CM_SAY").unwrap_or_default().split(',') {
        if let Some((k, v)) = pair.split_once('=') {
            let (k, v) = (k.trim(), v.trim());
            if !k.is_empty() {
                nivedana.said.insert(k.to_string(), v.to_string());
            }
        }
    }
    let mut job = Job {
        command: "echo hello".to_string(),
        workspace: None,
        cwd: None,
        env: Vec::new(),
        collect: Vec::new(),
    };
    let mut artifacts_dir = "cm-artifacts".to_string();
    let mut abandon = false;
    let mut rehearsing = false;
    let (mut repo, mut git_ref, mut dir, mut setup) = (None, None, None, None);

    // `--k=v` and `--k v` are the same thing to a person, so they are the same thing
    // here: split the joined form once, up front, rather than in every arm.
    let split: Vec<String> = args
        .iter()
        .flat_map(|a| match a.split_once('=') {
            Some((k, v)) if k.starts_with("--") => vec![k.to_string(), v.to_string()],
            _ => vec![(*a).to_string()],
        })
        .collect();
    let args: Vec<&str> = split.iter().map(String::as_str).collect();
    let args = args.as_slice();

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
            "--dry-run" => {
                // Ask what would happen and take nothing. Nobody's run is disturbed
                // to find out, which is what makes it usable on a busy fleet.
                rehearsing = true;
                i += 1;
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


            "--run" => {
                if let Some(cmd) = args.get(i + 1) {
                    job.command = (*cmd).to_string();
                }
                i += 2;
            }
            // Anything else is the caller talking to their own policy, not to cm.
            // `--plea nightly-regression`, `--incident INC-4471`, `--branch release-9`:
            // none of these are features here, and adding one would be cm guessing at a
            // vocabulary that belongs to the organisation. They travel as keys and values
            // and the tenant's files say what each is worth.
            //
            // Which does mean a mistyped cm flag becomes a declaration rather than an
            // error, so every one of them is echoed below: a `--dry-runn` that quietly
            // did nothing would be worse than either.
            other if other.starts_with("--") => {
                let key = other.trim_start_matches('-');
                match args.get(i + 1) {
                    // `--k v`
                    Some(v) if !v.starts_with("--") => {
                        nivedana.said.insert(key.to_string(), (*v).to_string());
                        i += 2;
                    }
                    // `--k` alone: a flag they wanted their policy to notice.
                    _ => {
                        nivedana.said.insert(key.to_string(), "true".into());
                        i += 1;
                    }
                }
            }
            other => bail!("unrecognised argument: {other}"),
        }
    }

    // Echoed because cm read none of it and cannot tell a deliberate key from a typo.
    if !nivedana.said.is_empty() {
        let said: Vec<String> =
            nivedana.said.iter().map(|(k, v)| format!("{k}={v}")).collect();
        println!("declaring: {}", said.join(" "));
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
        rehearsing,
    };
    let results = run(session).await?;
    Ok(exit_with(&results))
}

/// This device's name and key.
///
/// Exists because the alternative was telling an operator to grep `cm.toml` for the
/// key that `cm admin add` needs, and a config file is not an interface.
fn whoami() -> Result<()> {
    let home = home()?;
    let config = load_config(&home)?;
    println!("{} {}", config.name, config.key);
    Ok(())
}

// ---------------------------------------------------------------------------
// admins
// ---------------------------------------------------------------------------

/// Add an admin device, by key.
///
/// Local on purpose, like `cm tenant add`: this decides who may change how the
/// controller runs, so it is the host's act at the host's keyboard. The first admin
/// has to be added this way; there is no bootstrap in which a device grants itself
/// the power to grant power.
fn admin_add(name: &str, key: &str, args: &[&str]) -> Result<()> {
    let mut note = None;
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--note" => {
                note = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            other => bail!("unrecognised argument: {other}"),
        }
    }


    let config = load_config(&home()?)?;
    let mut admins = admin::Admins::load(&config.root)?;
    admins.add(admin::Admin { name: name.to_string(), key: key.to_string(), note })?;
    admins.save(&config.root)?;

    println!("admin `{name}` added");
    println!("  {key}");
    // Not re-read while running, deliberately — see the field's comment.
    println!("restart the controller for this to take effect.");
    Ok(())
}

fn admin_list() -> Result<()> {
    let config = load_config(&home()?)?;
    let admins = admin::Admins::load(&config.root)?;
    if admins.list.is_empty() {
        println!("no admins in {}", admin::Admins::path_in(&config.root).display());
        return Ok(());
    }
    for a in &admins.list {
        println!(
            "  {:<16} {}{}",
            a.name,
            a.key,
            a.note.as_ref().map(|n| format!("  — {n}")).unwrap_or_default()
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// tenants
// ---------------------------------------------------------------------------

/// Onboard a tenant, whose alias must be the name **our own sirji** knows them by.
///
/// Deliberately local: this writes files under the controller's root, and it is the
/// host's decision rather than a request anybody makes over the wire. When there is
/// a hosted product with an account system, that system calls this — it does not
/// become a plea.
fn tenant_add(alias: &str, args: &[&str]) -> Result<()> {
    let mut terms = tenant::Terms::default();
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--credits" => {
                terms.credits = args.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "--window" => {
                terms.window = args.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "--ceiling" => {
                terms.ceiling = args
                    .get(i + 1)
                    .and_then(|v| v.parse().ok())
                    .ok_or_else(|| anyhow::anyhow!("--ceiling wants a number"))?;
                i += 2;
            }
            "--admin" => {
                match args.get(i + 1) {
                    // Also a member, so `--admin dana` alone is a complete answer for a
                    // one-person tenant rather than half of one.
                    Some(who) => terms.admins.push((*who).to_string()),
                    None => bail!("--admin wants a caller alias"),
                }
                i += 2;
            }
            "--member" => {
                match args.get(i + 1) {
                    Some(who) => terms.members.push((*who).to_string()),
                    None => bail!("--member wants a caller alias"),
                }
                i += 2;
            }
            "--note" => {
                terms.note = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            other => bail!("unrecognised argument: {other}"),
        }
    }


    let config = load_config(&home()?)?;
    let dir = tenant::Tenants::add(&config.root, alias, terms.clone())?;

    // Read it straight back rather than describing what we just wrote: if it does
    // not load, the operator finds out now rather than when the tenant first asks.
    let tenants = tenant::Tenants::load(&config.root)?;
    let written = tenants
        .by_name(alias)
        .ok_or_else(|| anyhow::anyhow!("wrote {} but could not read it back", dir.display()))?;

    println!("tenant `{alias}` at {}", dir.display());
    println!("  ceiling {} machine(s)", written.terms.ceiling);
    println!("  members  {}", written.members().join(", "));
    match written.budget() {
        Some((c, w)) => println!("  budget   {c} credit(s) per rolling {w}s"),
        None => println!("  budget   none — machines are capped, spending is not"),
    }
    println!("  they edit {}", written.policy.path.display());
    println!("  you own  {}", dir.join(tenant::FILE).display());
    // No restart: the controller re-reads a tenant's folder when they next ask.
    println!("a running controller will pick this up on their next plea.");
    Ok(())
}

fn tenant_list() -> Result<()> {
    let config = load_config(&home()?)?;
    let tenants = tenant::Tenants::load(&config.root)?;
    if tenants.is_empty() {
        println!(
            "no tenants in {}",
            tenant::Tenants::dir_in(&config.root).display()
        );
        return Ok(());
    }
    for t in tenants.all() {
        {
            println!(
                "  {:<16} ceiling {:<5} grants last {:<6} budget {:<14} members {}{}",
                t.alias,
                t.terms.ceiling,
                format!("{}s", t.policy.reservation_secs()),
                match t.budget() {
                    Some((c, w)) => format!("{c}/{w}s"),
                    None => "none".to_string(),
                },
                t.members().join(","),
                t.terms
                    .note
                    .as_ref()
                    .map(|n| format!("  — {n}"))
                    .unwrap_or_default()
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// looking inside the controller
// ---------------------------------------------------------------------------

/// Ask our own controller what it is holding.
///
/// Run as **another device of the same organisation**, which is what earns the
/// answer: we resolve the controller through our shared parent with `ResolveLocal`,
/// and a ticket minted that way carries no alias, which is how the controller knows
/// we are one of its own rather than somebody's peer.
///
/// So this needs no new credential and no local socket — and it works when the
/// controller is on a machine the operator cannot log into.
/// `cm upload-policy <controller> [folder]`
///
/// Deliberately dumb about what it sends: the folder, as it is. Deciding here what a
/// policy may contain would put the rule in the one place that cannot enforce it — the
/// machine belonging to whoever is uploading.
async fn upload_policy(target: &str, dir: &std::path::Path) -> Result<std::process::ExitCode> {
    let up = upload::gather(dir)?;
    println!("sending {} file(s) from {}", up.files.len(), dir.display());
    for file in &up.files {
        println!("  {}", file.path);
    }

    let home = home()?;
    sirji::Settings::load(&home)?.activate();
    let config = load_config(&home)?;
    let secret = keys(&home).secret(&id52::decode(&config.key)?)?;

    let ask = match target.split_once('@') {
        Some((name, alias)) => sirji::proto::Ask::ResolveFor {
            name: name.to_string(),
            alias: alias.to_string(),
        },
        None => sirji::proto::Ask::ResolveLocal { name: target.to_string() },
    };
    let endpoint = sirji::endpoint::bind_dialer(secret).await?;
    let found =
        sirji::daemon::ask_as_device(&endpoint, &config.parent, &config.parent_hints, &ask).await?;
    let (device, ticket, hints) = match found {
        sirji::proto::Say::Resolved { device, ticket, hints } => (device, ticket, hints),
        sirji::proto::Say::No { reason } => bail!("cannot find `{target}`: {reason}"),
    };

    let conn = dial_any(&endpoint, id52::decode(&device)?, &hints).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut recv = BufReader::new(recv);
    write_line(&mut send, &Knock { ticket: Some(ticket), attestation: None }).await?;
    write_line(&mut send, &Plea::Upload(up)).await?;

    let outcome = match read_line::<Answer>(&mut recv).await? {
        Some(Answer::Decided(Verdict::Ok)) => {
            println!("replaced — the next plea is weighed against this");
            Ok(std::process::ExitCode::SUCCESS)
        }
        Some(Answer::Decided(Verdict::Deny { rationale })) => {
            // Not an error to raise: the controller decided, and said why. Printing it as
            // a refusal keeps "you may not" apart from "something broke".
            eprintln!("refused: {rationale}");
            Ok(std::process::ExitCode::FAILURE)
        }
        Some(Answer::Fault { fault }) => bail!("the controller faulted: {fault}"),
        other => bail!("unexpected answer: {other:?}"),
    };

    let _ = send.finish();
    conn.close(0u32.into(), b"done");
    endpoint.close().await;
    outcome
}

async fn inspect(what: proto::Look, args: &[&str]) -> Result<()> {
    let mut name = "cm-c".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--controller" => {
                name = args.get(i + 1).unwrap_or(&"cm-c").to_string();
                i += 2;
            }
            other => bail!("unrecognised argument: {other}"),
        }
    }


    let home = home()?;
    sirji::Settings::load(&home)?.activate();
    let config = load_config(&home)?;
    let secret = keys(&home).secret(&id52::decode(&config.key)?)?;

    // A bare name is a sibling of ours; `name@org` is somebody else's controller.
    // Both are askable — the *controller* decides who may look, not this command.
    // Refusing locally would put the answer in the wrong place and produce a
    // misleading error, since a peer's controller is perfectly reachable.
    let ask = match name.split_once('@') {
        Some((name, alias)) => sirji::proto::Ask::ResolveFor {
            name: name.to_string(),
            alias: alias.to_string(),
        },
        None => sirji::proto::Ask::ResolveLocal { name: name.clone() },
    };

    let endpoint = sirji::endpoint::bind_dialer(secret).await?;
    let found =
        sirji::daemon::ask_as_device(&endpoint, &config.parent, &config.parent_hints, &ask).await?;
    let (device, ticket, hints) = match found {
        sirji::proto::Say::Resolved { device, ticket, hints } => (device, ticket, hints),
        sirji::proto::Say::No { reason } => bail!("cannot find `{name}`: {reason}"),
    };

    let conn = dial_any(&endpoint, id52::decode(&device)?, &hints).await?;
    let (mut send, recv) = conn.open_bi().await?;
    let mut recv = BufReader::new(recv);

    write_line(&mut send, &Knock { ticket: Some(ticket), attestation: None }).await?;
    write_line(&mut send, &Plea::Inspect { what }).await?;

    let outcome = match read_line::<Answer>(&mut recv).await? {
        Some(Answer::Decided(Verdict::Saw(sight))) => {
            for line in sight.lines {
                println!("{line}");
            }
            Ok(())
        }
        Some(Answer::Decided(Verdict::Deny { rationale })) => Err(anyhow::anyhow!("{rationale}")),
        Some(Answer::Fault { fault }) => Err(anyhow::anyhow!("the controller faulted: {fault}")),
        other => Err(anyhow::anyhow!("unexpected answer: {other:?}")),
    };

    let _ = send.finish();
    conn.close(0u32.into(), b"done");
    endpoint.close().await;
    outcome
}

// ---------------------------------------------------------------------------
// ping
// ---------------------------------------------------------------------------

/// Walk the whole chain and say which link is broken.
///
/// Everything here is exercised by a real run too, but a real run reports only that
/// it failed. This reports *where*: the four hops have four completely different
/// fixes, and telling them apart is the entire point. Costs nobody a machine.
async fn ping(target: &str) -> Result<std::process::ExitCode> {
    fn ok(step: &str, detail: impl std::fmt::Display) {
        println!("  ok    {step:<22} {detail}");
    }
    fn bad(step: &str, detail: impl std::fmt::Display) -> std::process::ExitCode {
        println!("  FAIL  {step:<22} {detail}");
        std::process::ExitCode::FAILURE
    }

    println!("pinging {target}");

    // 1. Ourselves. A device with no home or no key never got as far as enrolling.
    let home = home()?;
    sirji::Settings::load(&home)?.activate();
    let config = match load_config(&home) {
        Ok(config) => config,
        Err(e) => {
            return Ok(bad(
                "identity",
                format!("{}: {e} — has `cm init` run here?", home.display()),
            ));
        }
    };
    let key = id52::decode(&config.key)?;
    let secret = match keys(&home).secret(&key) {
        Ok(secret) => secret,
        Err(e) => return Ok(bad("identity", format!("cannot read our own key: {e:#}"))),
    };
    ok("identity", format!("`{}` is {}", config.name, config.key));

    let (name, org) = match target.split_once('@') {
        Some(parts) => parts,
        None => return Ok(bad("target", format!("{target:?} is not name@org"))),
    };

    let endpoint = sirji::endpoint::bind_dialer(secret).await?;
    let verdict = async {
        // 2. Our own sirji. Everything else goes through it, so a failure here is
        // never about the other organisation.
        let parent = match config.parent.first() {
            Some(address) => id52::decode(address)?,
            None => return Ok(bad("our sirji", "no parent address in cm.toml")),
        };
        match dial_any(&endpoint, parent, &config.parent_hints).await {
            Ok(conn) => {
                conn.close(0u32.into(), b"ping");
                ok("our sirji", format!("reached {}", config.parent[0]));
            }
            Err(e) => {
                return Ok(bad(
                    "our sirji",
                    format!("{e:#} — is the daemon running (`sirji daemon`)?"),
                ));
            }
        }

        // 3. The name. One request, but three ways to fail, and the daemon's own
        // words distinguish them: we do not know that organisation, they have no
        // device by that name, or the device is not connected.
        let resolved = sirji::daemon::ask_as_device(
            &endpoint,
            &config.parent,
            &config.parent_hints,
            &sirji::proto::Ask::ResolveFor {
                name: name.to_string(),
                alias: org.to_string(),
            },
        )
        .await;
        let (device, ticket, hints) = match resolved {
            Ok(sirji::proto::Say::Resolved { device, ticket, hints }) => (device, ticket, hints),
            Ok(sirji::proto::Say::No { reason }) => return Ok(bad("resolve", reason)),
            Err(e) => return Ok(bad("resolve", format!("{e:#}"))),
        };
        ok("resolve", format!("{target} is {device}"));

        // 4. The controller itself, dialled directly. The hop most likely to be a
        // network problem rather than a configuration one.
        let conn = match dial_any(&endpoint, id52::decode(&device)?, &hints).await {
            Ok(conn) => conn,
            Err(e) => {
                let where_ = if hints.is_empty() {
                    "no hints, so this needed discovery".to_string()
                } else {
                    format!("tried {hints:?}, then discovery")
                };
                return Ok(bad("dial", format!("{e:#} — {where_}")));
            }
        };
        ok(
            "dial",
            if hints.is_empty() { "found by discovery".to_string() } else { format!("{hints:?}") },
        );

        // 5. The ticket. Minted by our sirji, verified by the controller against its
        // own parent's signature — which is the only reason it will talk to us.
        let (mut send, recv) = conn.open_bi().await?;
        let mut recv = BufReader::new(recv);
        write_line(&mut send, &Knock { ticket: Some(ticket), attestation: None }).await?;
        write_line(&mut send, &Plea::Ping).await?;

        let verdict = match read_line::<Answer>(&mut recv).await {
            Ok(Some(Answer::Decided(verdict))) => verdict,
            // A fault here is the controller's to fix, and naming it that way is the
            // difference between "check your credentials" and "check your controller".
            Ok(Some(Answer::Fault { fault })) => return Ok(bad("controller", fault)),
            Ok(None) => return Ok(bad("auth", "the controller hung up without answering")),
            Err(e) => return Ok(bad("auth", format!("{e:#}"))),
        };
        let outcome = match verdict {
            Verdict::Pong => {
                ok("auth", "the controller accepted our ticket");
                // No fleet line. What is here is the controller's business; what a
                // caller may have is answered by asking, and `--dry-run` answers it
                // without taking anything.
                std::process::ExitCode::SUCCESS
            }
            Verdict::Deny { rationale } => bad("auth", rationale),
            other => bad("auth", format!("unexpected answer: {other:?}")),
        };

        let _ = send.finish();
        conn.close(0u32.into(), b"done");
        Ok(outcome)
    }
    .await;

    endpoint.close().await;
    verdict
}

/// One complete errand: ask, use what you are given, give it back.
struct Session {
    target: String,
    nivedana: Nivedana,
    job: Job,
    artifacts_dir: String,
    abandon: bool,
    /// Ask what would happen, run nothing, hold nothing.
    rehearsing: bool,
}

/// What a session came back with. Empty when the plea was countered — the caller
/// was told why, and nothing ran.
type Results = Vec<Result<Shard>>;

/// Resolve, plead, dispatch, release.
///
/// The whole of what cm offers a caller. Everything a test runner needs on top of
/// this — shard arithmetic, report formats, merging — belongs to that runner's
/// The enrolled way: our own sirji resolves the controller and mints us a ticket.
async fn enrolled(
    name: &str,
    org: &str,
    target: &str,
) -> Result<(sirji::Endpoint, sirji::Connection, Knock)> {
    let home = home()?;
    sirji::Settings::load(&home)?.activate();
    let config = load_config(&home)?;
    let secret = keys(&home).secret(&id52::decode(&config.key)?)?;

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
    Ok((endpoint, conn, Knock { ticket: Some(ticket), attestation: None }))
}

pub const ATTEST_ENV: &str = "CM_ATTEST";
pub const MINT_ENV: &str = "CM_ATTEST_CMD";
pub const HINTS_ENV: &str = "CM_CONTROLLER_HINTS";

/// The attested way: nothing enrolled, and nothing left behind.
///
/// A key minted for this run, a token whose audience *is* that key, and a direct dial. No
/// `cm init`, no parent, no entry in anybody's roster — which is the point, because a
/// runner that enrolled would leave one dead entry per build.
///
/// The order matters and is the whole security property: **the key exists before the token
/// is asked for**, because the audience has to be the key. A token fetched first could only
/// be a bearer token, and a bearer token in a build log is a credential anybody can replay.
/// Anything with a dot or a scheme. Deliberately loose: the alternative is refusing a
/// perfectly good name because it did not look like one, and a wrong guess here fails
/// immediately and legibly rather than quietly.
fn looks_like_a_host(target: &str) -> bool {
    target.starts_with("https://") || target.starts_with("http://") || target.contains('.')
}

/// Ask a host which controller it runs.
///
/// `http://` is accepted because a scenario and a private network both need it, and
/// refusing it would only mean somebody tunnels around the refusal. It is a downgrade and
/// says so.
async fn discover(target: &str) -> Result<Published> {
    let url = if target.contains("://") {
        // A full URL may already be the document, or just the host.
        if target.contains(WELL_KNOWN) {
            target.to_string()
        } else {
            format!("{}{WELL_KNOWN}", target.trim_end_matches('/'))
        }
    } else {
        format!("https://{target}{WELL_KNOWN}")
    };
    if url.starts_with("http://") {
        eprintln!("warning: {url} is not https, so nothing vouches for this answer");
    }

    reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .with_context(|| format!("asking {url} which controller to use"))?
        .error_for_status()?
        .json()
        .await
        .with_context(|| format!("{url} did not answer with a controller document"))
}

async fn attesting(
    target: &str,
    published: Vec<String>,
) -> Result<(sirji::Endpoint, sirji::Connection, Knock)> {
    let secret = sirji::SecretKey::generate();
    let mine = id52::encode(&secret.public());
    let endpoint = sirji::endpoint::bind_dialer(secret).await?;

    let token = token_for(&mine).await?;
    eprintln!("attesting as a one-off key {mine}");

    // Dialled by key. There is no parent to resolve through, so the controller's id52 is
    // the address — from a CI variable, or from DNS once that exists.
    // What the host published, plus anything the environment overrode it with. The
    // variable wins because it is the one somebody sets when discovery is wrong.
    let mut hints: Vec<String> = std::env::var(HINTS_ENV)
        .unwrap_or_default()
        .split(',')
        .filter(|h| !h.trim().is_empty())
        .map(|h| h.trim().to_string())
        .collect();
    hints.extend(published);
    let conn = dial_any(&endpoint, id52::decode(target)?, &hints).await?;
    Ok((endpoint, conn, Knock { ticket: None, attestation: Some(token) }))
}

/// A token whose audience is `mine`.
///
/// Note what is *not* offered: a variable holding a ready-made token. It could never be
/// right — the audience has to be a key that does not exist until this run starts, so a
/// token prepared in advance is either for the wrong audience or is a bearer token, and cm
/// does not accept bearer tokens.
///
/// So the escape hatch is a **command**, not a value. `CM_ATTEST_CMD` is run with
/// `{audience}` replaced by this run's key, and whatever it prints is the token. One hook
/// covers every provider cm has no integration with, which is the same reason the enrolment
/// flow implements protocols rather than providers.
///
/// Nothing about the token is checked here. The controller is the only party whose opinion
/// of it matters, and a client that pre-validated would be a second implementation to
/// disagree with the first.
async fn token_for(mine: &str) -> Result<String> {
    if let Ok(template) = std::env::var(MINT_ENV).map(|t| t.trim().to_string())
        && !template.is_empty()
    {
        let command = template.replace("{audience}", mine);
        let out = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .output()
            .await
            .with_context(|| format!("running {MINT_ENV}: {command}"))?;
        if !out.status.success() {
            bail!(
                "{MINT_ENV} failed ({}): {}",
                describe(out.status.code()),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if token.is_empty() {
            bail!("{MINT_ENV} printed nothing");
        }
        return Ok(token);
    }

    let which = std::env::var(ATTEST_ENV).unwrap_or_else(|_| "github".into());
    match which.as_str() {
        // GitHub hands out a token per audience, which is exactly the shape needed. Both
        // variables appear only when a workflow requests `id-token: write`, so their
        // absence is a permissions problem and worth saying so.
        "github" => {
            let (url, bearer) = (
                std::env::var("ACTIONS_ID_TOKEN_REQUEST_URL").ok(),
                std::env::var("ACTIONS_ID_TOKEN_REQUEST_TOKEN").ok(),
            );
            let (Some(url), Some(bearer)) = (url, bearer) else {
                bail!(
                    "no way to prove who this is. On GitHub Actions, give the job \
                     `permissions: id-token: write`; elsewhere set {MINT_ENV} to a command \
                     that prints a token for {{audience}}; or use `name@org` with an \
                     enrolled device."
                )
            };
            #[derive(serde::Deserialize)]
            struct Minted {
                value: String,
            }
            let minted: Minted = reqwest::Client::new()
                .get(format!("{url}&audience={mine}"))
                .bearer_auth(bearer)
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
                .context("asking GitHub for a token")?
                .error_for_status()?
                .json()
                .await
                .context("GitHub's answer was not the shape expected")?;
            Ok(minted.value)
        }
        other => bail!("no integration for {ATTEST_ENV}={other:?}; set {MINT_ENV} instead"),
    }
}

/// plugin, not here.
async fn run(session: Session) -> Result<Results> {
    // Two ways to be somebody, and which one applies is decided by the target. `name@org`
    // means we have a parent that can resolve it and mint us a ticket. A bare id52 means
    // we do not — a CI runner with nothing enrolled — so we prove ourselves with a token
    // instead and dial the controller directly.
    let (endpoint, conn, knock) = match session.target.split_once('@') {
        Some((name, org)) => enrolled(name, org, &session.target).await?,
        // An id52 names the controller directly.
        None if id52::decode(&session.target).is_ok() => {
            attesting(&session.target, Vec::new()).await?
        }
        // A host name means "ask it who it is", which is what a CI variable should hold:
        // a key rotates, a name does not.
        None if looks_like_a_host(&session.target) => {
            let found = discover(&session.target).await?;
            eprintln!("{} publishes {}", session.target, found.key);
            attesting(&found.key, found.hints).await?
        }
        None => bail!(
            "{:?} is none of the three ways to name a controller: `name@org` from an \
             enrolled device, a host that publishes {WELL_KNOWN}, or the id52 itself.",
            session.target
        ),
    };
    let (mut send, recv) = conn.open_bi().await?;
    let mut recv = BufReader::new(recv);

    // Everything from here has to unwind the same way, whatever the answer was:
    // a counter or a refusal that skipped the close left iroh's tasks to be
    // cancelled out from under the runtime, which surfaced as a panic on exit.
    let outcome = async {
        write_line(&mut send, &knock).await?;
        let plea = if session.rehearsing {
            Plea::Rehearse(session.nivedana)
        } else {
            Plea::Nivedana(session.nivedana)
        };
        write_line(&mut send, &plea).await?;

        let verdict = match read_line::<Answer>(&mut recv)
            .await?
            .ok_or_else(|| anyhow::anyhow!("the controller said nothing"))?
        {
            Answer::Decided(verdict) => verdict,
            // Nothing was granted, refused or held. Not a refusal: nobody read the
            // request, so the fix is the controller's, and saying "denied" would send
            // the caller off to argue with a policy that was never consulted.
            Answer::Fault { fault } => bail!(
                "the controller could not weigh this request: {fault}\n  \
                 nothing was refused, and nothing was allocated. Tell whoever runs it."
            ),
        };

        let (reservation, workers, expires_in) = match verdict {
            Verdict::Would { count, rationale } => {
                // Nothing was held, so there is nothing to release and nothing to
                // run. Said in the present tense on purpose: it is a snapshot, and
                // the fleet will have moved by the time they ask for real.
                println!("would get {count} machine(s) — {rationale}");
                return Ok(Vec::new());
            }
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
            other => bail!("expected a verdict on our plea, got {other:?}"),
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
        match read_line::<Answer>(&mut recv).await? {
            Some(Answer::Decided(Verdict::Ok)) => println!("released {reservation}"),
            Some(Answer::Decided(Verdict::Deny { rationale })) => {
                eprintln!("release refused: {rationale}")
            }
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
        let dir = cyberium::testing::scratch("artifacts");
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
        let dir = cyberium::testing::scratch("artifacts-two");
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
    fn a_controller_can_be_named_three_ways() {
        // `name@org` from an enrolled device; a host that publishes the document; or the
        // key itself. Anything else fails immediately and names all three, rather than
        // reaching id52 decoding with a string nobody meant as a key.
        assert!(looks_like_a_host("cm.acme.com"));
        assert!(looks_like_a_host("https://cm.acme.com"));
        assert!(looks_like_a_host("http://127.0.0.1:8822"));
        assert!(!looks_like_a_host("cm-c"), "a bare word is not a host");
        // An id52 has no dot, so the two forms never overlap.
        assert!(!looks_like_a_host("5lljf7j7vvvj8pmnd9j1uh82lb984j3bmifs12n0qqeens6mfkpg"));
    }

    #[test]
    fn nothing_the_caller_says_is_read_by_cm() {
        // The whole of the reshape: `plea`, `why`, `incident`, `role` are keys with no
        // meaning here. Earlier versions had each of them as a field, and each addition
        // was cm guessing at a vocabulary belonging to the organisation — which is how a
        // rule like "writing one plea turns free text off" ended up compiled in.
        let said = declarations(&[
            "--plea", "nightly-regression",
            "--incident", "INC-4471",
            "--urgent",
        ]);
        assert_eq!(said.get("plea").map(String::as_str), Some("nightly-regression"));
        assert_eq!(said.get("incident").map(String::as_str), Some("INC-4471"));
        // A bare flag is a fact the caller wanted their policy to notice.
        assert_eq!(said.get("urgent").map(String::as_str), Some("true"));
    }

    #[test]
    fn a_key_may_be_joined_or_separated() {
        // The same thing to a person, so the same thing here.
        assert_eq!(declarations(&["--plea=nightly"]), declarations(&["--plea", "nightly"]));
    }

    #[test]
    fn the_operational_arguments_are_still_cms_own() {
        // `--count` and `--need` are not opinions: one bounds the grant, the other picks
        // machines that can do the work. Neither may be swallowed as a declaration.
        let said = declarations(&["--count", "3", "--need", "linux", "--dry-run"]);
        assert!(said.is_empty(), "{said:?}");
    }

    /// Parse just the declaration arm the way `test` does, without a network.
    fn declarations(args: &[&str]) -> std::collections::BTreeMap<String, String> {
        let split: Vec<String> = args
            .iter()
            .flat_map(|a| match a.split_once('=') {
                Some((k, v)) if k.starts_with("--") => vec![k.to_string(), v.to_string()],
                _ => vec![(*a).to_string()],
            })
            .collect();
        let known = ["--count", "--need", "--run", "--dry-run", "--repo", "--ref", "--dir"];
        let mut said = std::collections::BTreeMap::new();
        let mut i = 0;
        while i < split.len() {
            let arg = split[i].as_str();
            if known.contains(&arg) {
                i += if arg == "--dry-run" { 1 } else { 2 };
                continue;
            }
            if let Some(key) = arg.strip_prefix("--") {
                match split.get(i + 1) {
                    Some(v) if !v.starts_with("--") => {
                        said.insert(key.to_string(), v.clone());
                        i += 2;
                    }
                    _ => {
                        said.insert(key.to_string(), "true".into());
                        i += 1;
                    }
                }
                continue;
            }
            i += 1;
        }
        said
    }








    fn held(state: State) -> Held {
        Held { caller: "them".into(), limits: Limits::default(), state }
    }

    #[test]
    fn a_machine_that_could_not_be_prepared_refuses_work() {
        // The point of the whole mechanism: rather than lend out a box whose
        // cleanup failed, say why and refuse.
        let verdict = consider(Some(&held(State::Unusable("disk full".into()))), "r1", "them");
        let Admission::Refuse(why) = verdict else {
            panic!("expected a refusal, got {verdict:?}");
        };
        assert!(why.contains("could not be prepared"), "{why}");
        assert!(why.contains("disk full"), "{why}");
    }

    #[test]
    fn work_waits_while_the_machine_is_being_prepared() {
        // Not a refusal: the controller tells the machine and the caller about a
        // grant at the same moment, so a caller that dialled promptly would
        // otherwise be turned away for being on time.
        let verdict = consider(Some(&held(State::Preparing)), "r1", "them");
        assert!(matches!(verdict, Admission::Wait), "{verdict:?}");
    }

    #[test]
    fn a_prepared_machine_serves_the_right_caller_only() {
        assert!(matches!(
            consider(Some(&held(State::Ready)), "r1", "them"),
            Admission::Run(_)
        ));
        let verdict = consider(Some(&held(State::Ready)), "r1", "somebody-else");
        let Admission::Refuse(why) = verdict else {
            panic!("expected a refusal, got {verdict:?}");
        };
        assert!(why.contains("not yours"), "{why}");
        // And a reservation we never heard of is refused whoever asks.
        assert!(matches!(consider(None, "r9", "them"), Admission::Refuse(_)));
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

