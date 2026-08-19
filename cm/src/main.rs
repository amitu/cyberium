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

mod admin;
mod adviser;
mod budget;
mod fleet;
mod nivedana;
mod policy;
mod proto;
mod tenant;
#[cfg(test)]
mod testing;

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use fleet::{Fleet, Shortfall, Worker};
use policy::Ruling;
use proto::{
    Aadesh, Answer, Artifact, Limits, Nivedana, Outcome, Plea, Register, Upadesh, Verdict,
    WorkerHandle,
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
                       [--member <alias>]... [--note <text>]
  cm tenant list
        onboard whoever this controller serves — always, self-hosted too, where
        a tenant is usually a team. Members are the caller aliases our own sirji
        knows them by; with none given, the tenant's own name is its only member.
        They write policy.md; you write tenant.toml, which is what stops a
        tenant setting its own quota or claiming somebody else's callers.

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

/// Is this text actually an id52? Used wherever a key arrives as a string from a
/// human, so a typo is caught where it was typed rather than at the first dial.
fn id52_check(key: &str) -> Result<()> {
    id52::decode(key).map(|_| ()).with_context(|| format!("{key:?} is not an id52"))
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

async fn controller() -> Result<()> {
    let home = home()?;
    sirji::Settings::load(&home)?.activate();
    let config = load_config(&home)?;

    // Every tenant is read and validated here, not lazily: a policy that does not
    // parse should stop the controller starting, rather than surface an hour later
    // on somebody's first plea with them waiting on the other end.
    let tenants = tenant::Tenants::load(&config.root)?;
    println!("controller `{}` listening as {}", config.name, config.key);
    if tenants.len() == 0 {
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
            prose,
            catalogue,
            chosen,
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

            // Which plea this is, checked against the ones this organisation wrote
            // down. Deterministic, and before the model: an alias that is not in the
            // catalogue is not a question of interpretation, and refusing here keeps
            // the caller's string off the model's input path entirely.
            let chosen = match resolve_plea(&tenant.nivedanas, nivedana) {
                Ok(chosen) => chosen,
                Err(rationale) => return refused(rationale),
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
                tenant.policy.prose(),
                tenant.nivedanas.catalogue(),
                chosen,
                tenant.policy.reservation_secs(),
                tenant.ceiling(),
                tenant.budget(),
                standing,
                ceiling,
                wanted,
            )
        };

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
            prose,
            catalogue,
            attested: adviser::Attested {
                tenant: tenant_name.clone(),
                caller: alias.to_string(),
            },
            declared: adviser::Declared {
                plea: chosen,
                why: nivedana.why.clone(),
                context: nivedana.context.clone(),
                count: wanted,
                capabilities: nivedana.capabilities.clone(),
                role: nivedana.role.clone(),
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
        let opinion = self
            .adviser
            .weigh(&advice)
            .await
            .with_context(|| format!("weighing {alias}'s plea"))?;

        if opinion.denies() {
            println!("policy refused {alias}: {}", opinion.rationale);
            return refused(opinion.rationale);
        }

        let said = opinion.count;
        let mut allowed = opinion.bounded(&advice);
        let mut rationale = opinion.rationale.clone();
        println!(
            "policy weighed {alias}: asked {wanted}, said {said}, giving {allowed} — {rationale}"
        );

        // The sanity net, applied in one pure place so it can be tested without a
        // controller, a fleet or a network.
        let (cut, faults) = sanity(
            said,
            allowed,
            Limit {
                ceiling: ceiling.min(host_ceiling),
                free: brief.free,
                host: host_ceiling,
                affordable: money.as_ref().map(|m| {
                    let rates: Vec<u32> =
                        brief.rates.iter().copied().take(allowed as usize).collect();
                    budget::Room { budget: m.budget, spent: m.spent, committed: m.committed }
                        .affordable(&rates, lifetime)
                }),
            },
        );
        allowed = cut;

        if !faults.is_empty() {
            // Loudly. A policy that keeps overshooting is a policy nobody has noticed
            // is wrong, because the clamp has been quietly making it look right.
            eprintln!(
                "policy for {tenant_name} overshot limits it was shown ({}) — cut to \
                 {allowed}. Fix the policy; the prompt stated every one of them.",
                faults.join("; ")
            );
            // The model's sentence argued for a number the caller is not getting, so
            // it is not an explanation of the one they are.
            rationale = format!("{allowed} machine(s): {}", faults.join("; "));
        }

        if allowed == 0 {
            // In cm's own words. The model's sentence argued for a number nobody is
            // getting, and printing it beside a refusal produced denials that read
            // "granted what was asked" — true of the argument, false of the outcome.
            return refused(if faults.is_empty() {
                format!("this policy granted none of the {wanted} asked for")
            } else {
                faults.join("; ")
            });
        }

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
struct Caller {
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

/// Which of the organisation's pleas this request is, if it named one.
///
/// Returns the alias as the catalogue spells it, so what reaches the prompt is the
/// organisation's own key rather than the caller's spelling of it.
///
/// Once a tenant has written any plea down, free text stops being heard from it. That
/// is the whole point: it is how an organisation takes the one caller-written string
/// off the input path of its own decisions, and writing the file is how it opts in —
/// there is no flag to forget.
fn resolve_plea(
    known: &nivedana::Nivedanas,
    asked: &Nivedana,
) -> std::result::Result<Option<String>, String> {
    let named = asked.plea.as_deref().map(str::trim).filter(|p| !p.is_empty());

    if known.is_empty() {
        // Nothing written down. A named plea cannot be honoured, and quietly ignoring
        // it would weigh the request against something other than what was asked.
        if let Some(name) = named {
            return Err(format!(
                "this tenant has no `{}/` — nothing here is called `{name}`",
                nivedana::DIR
            ));
        }
        return Ok(None);
    }

    let Some(name) = named else {
        return Err(format!(
            "this tenant answers named pleas only. Name one with `--plea`: {}",
            known.aliases().join(", ")
        ));
    };
    let Some(_) = known.get(name) else {
        // The list, not just the news that they were wrong.
        return Err(format!(
            "no plea called `{name}` here. There is: {}",
            known.aliases().join(", ")
        ));
    };
    // Free text alongside a named plea is refused rather than dropped: dropping it
    // would silently weigh the request against different words than the caller thinks.
    if asked.why.as_deref().is_some_and(|w| !w.trim().is_empty()) {
        return Err(format!(
            "`{name}` is a named plea, so the free-text reason cannot also be heard — \
             put anything extra in the context JSON, where policy can require it"
        ));
    }
    Ok(Some(nivedana::alias_of(name)))
}

/// What policy and the fleet concluded: a number, or a verdict to send instead.
///
/// Wrapped in a `Result` by its callers, whose `Err` means something different again —
/// that nothing was concluded at all. Three outcomes, and the type says which is which:
/// `Ok(Ok(n))` you may have n, `Ok(Err(v))` here is why not, `Err(e)` nobody decided.
type Entitled = std::result::Result<(u32, Option<String>), Verdict>;

/// A refusal is a decision, so it is an `Ok` carrying a verdict.
fn refused(rationale: String) -> Result<Entitled> {
    Ok(Err(Verdict::Deny { rationale }))
}

/// Every bound that is re-checked after the model has answered.
///
/// All four were in the prompt. This exists for the case where the answer ignored one
/// anyway — a policy that argues past its own ceiling, a model that misreads a
/// number, a fleet that emptied between the brief and the answer.
struct Limit {
    /// The organisation's own `max_limit`.
    ceiling: u32,
    /// Machines free right now.
    free: u32,
    /// The host's cap on this tenant, whatever their policy says.
    host: u32,
    /// What the budget can buy, when there is one.
    affordable: Option<u32>,
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
fn sanity(said: u32, bounded: u32, limit: Limit) -> (u32, Vec<String>) {
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
async fn rehearse(control: &Control, alias: &str, nivedana: &Nivedana) -> Result<Verdict> {
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

async fn decide(
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

async fn test(
    target: &str,
    why: Option<&str>,
    args: &[&str],
) -> Result<std::process::ExitCode> {
    // The positional reason is free text, so it is only heard by tenants that wrote no
    // pleas of their own. `--plea` is the other way in, and the one CI should use.
    let mut nivedana = Nivedana {
        why: why.map(str::to_string).filter(|w| !w.trim().is_empty()),
        plea: std::env::var("CM_PLEA").ok().filter(|p| !p.trim().is_empty()),
        role: std::env::var("CM_T_ROLE").ok(),
        ..Default::default()
    };
    // Whatever the caller's CI wants the policy to see. Parsed here rather than passed
    // through as a string, so a job with a broken template is told so by the tool it
    // ran instead of by a refusal it cannot read.
    if let Some(raw) = std::env::var("CM_CONTEXT").ok().filter(|c| !c.trim().is_empty()) {
        nivedana.context = Some(
            serde_json::from_str(&raw)
                .with_context(|| format!("CM_CONTEXT is not JSON: {raw:?}"))?,
        );
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
            "--role" => {
                nivedana.role = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            "--plea" => {
                nivedana.plea = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            "--context" => {
                let raw = args.get(i + 1).copied().unwrap_or_default();
                nivedana.context = Some(
                    serde_json::from_str(raw)
                        .with_context(|| format!("--context is not JSON: {raw:?}"))?,
                );
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
    if tenants.len() == 0 {
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

    write_line(&mut send, &Knock { ticket: Some(ticket) }).await?;
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
        write_line(&mut send, &Knock { ticket: Some(ticket) }).await?;
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
        let dir = testing::scratch("artifacts");
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
        let dir = testing::scratch("artifacts-two");
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

    fn pleas(entries: &[(&str, &str)]) -> nivedana::Nivedanas {
        let root = testing::scratch("plea");
        std::fs::create_dir_all(root.join(nivedana::DIR)).unwrap();
        let text: String =
            entries.iter().map(|(h, b)| format!("## {h}\n\n{b}\n\n")).collect();
        std::fs::write(root.join(nivedana::DIR).join("p.md"), text).unwrap();
        let loaded = nivedana::Nivedanas::load(&root).unwrap();
        std::fs::remove_dir_all(&root).ok();
        loaded
    }

    fn asking(plea: Option<&str>, why: Option<&str>) -> Nivedana {
        Nivedana {
            plea: plea.map(str::to_string),
            why: why.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn with_no_pleas_written_down_free_text_is_still_heard() {
        // The zero-config path has to keep working: a tenant that never wrote a
        // `nivedanas/` is not opting into anything.
        let none = nivedana::Nivedanas::default();
        assert_eq!(resolve_plea(&none, &asking(None, Some("an incident"))), Ok(None));
    }

    #[test]
    fn naming_a_plea_that_does_not_exist_is_refused_not_ignored() {
        // Ignoring it would weigh the request against something other than what was
        // asked for, and the caller would never know which.
        let none = nivedana::Nivedanas::default();
        let e = resolve_plea(&none, &asking(Some("nightly"), None)).unwrap_err();
        assert!(e.contains("nothing here is called `nightly`"), "{e}");

        let known = pleas(&[("nightly regression", "Routine.")]);
        let e = resolve_plea(&known, &asking(Some("noctural"), None)).unwrap_err();
        assert!(e.contains("no plea called `noctural`"), "{e}");
        // With the list, because somebody who guessed wrong needs the answer.
        assert!(e.contains("nightly-regression"), "{e}");
    }

    #[test]
    fn writing_pleas_down_stops_free_text_being_heard_at_all() {
        // The security property: with a catalogue, the only caller-written string on
        // the model's input path is an alias that was checked against a list.
        let known = pleas(&[("nightly regression", "Routine.")]);
        let e = resolve_plea(&known, &asking(None, Some("just trust me"))).unwrap_err();
        assert!(e.contains("named pleas only"), "{e}");
        assert!(e.contains("nightly-regression"), "the refusal names what they can ask for");
    }

    #[test]
    fn free_text_alongside_a_named_plea_is_refused_rather_than_dropped() {
        // Dropping it silently would weigh the request against different words than the
        // caller believes they sent.
        let known = pleas(&[("nightly regression", "Routine.")]);
        let e = resolve_plea(&known, &asking(Some("nightly-regression"), Some("also urgent!")))
            .unwrap_err();
        assert!(e.contains("cannot also be heard"), "{e}");
    }

    #[test]
    fn the_prompt_gets_the_catalogue_s_spelling_not_the_caller_s() {
        // Whatever they typed, what reaches the model is the organisation's own key —
        // so a policy that names a plea and a request that picked it agree.
        let known = pleas(&[("Nightly Regression", "Routine.")]);
        assert_eq!(
            resolve_plea(&known, &asking(Some("NIGHTLY_regression"), None)),
            Ok(Some("nightly-regression".to_string()))
        );
    }

    #[test]
    fn an_empty_plea_argument_is_the_same_as_none() {
        // `--plea "$CM_PLEA"` with the variable unset is a blank string, not a request
        // to find a plea called "".
        let known = pleas(&[("nightly", "Routine.")]);
        let e = resolve_plea(&known, &asking(Some("  "), None)).unwrap_err();
        assert!(e.contains("named pleas only"), "{e}");
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

    fn a_key() -> String {
        sirji::id52::encode(&sirji::SecretKey::generate().public())
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
