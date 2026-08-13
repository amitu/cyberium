//! `cm` — cost-aware allocation of test machines, over sirji.
//!
//! Two roles so far, both sirji **devices**:
//!
//! - `cm controller` answers to a name at an organisation's sirji. Anyone that
//!   organisation has a relationship with can resolve it.
//! - `cm test` is a device of the developer's own sirji. It resolves the
//!   controller by name, which gets it a ticket, and sends a plea.
//!
//! Neither holds any identity state. The controller learns who is asking from the
//! ticket its parent signed; the tester never learns anything about the
//! controller's organisation at all.

mod policy;
mod proto;

use anyhow::{Result, bail};
use proto::{Nivedana, Verdict};
use sirji::id52;
use tokio::io::{AsyncBufReadExt, BufReader};

const USAGE: &str = "\
cm — cost-aware allocation of test machines

  cm init --parent <invite> [--root <dir>]
        create $CM_HOME and enrol with the sirji that issued the invite.
        Get one with `sirji device invite <name>`.

  cm controller
        answer pleas, weighing each against policy.md

  cm test <name@org> \"<why>\" [--count N] [--class C] [--role R]
        ask for machines, and print the verdict

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
// where this device lives
// ---------------------------------------------------------------------------

const HOME_ENV: &str = "CM_HOME";
const HOME_DEFAULT: &str = ".cm";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Config {
    /// The name we answer to at our parent.
    name: String,
    /// Our own key. We listen on it; a caller that resolved our name dials it.
    key: String,
    /// Our parent's addresses.
    parent: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    parent_hints: Vec<String>,
    /// Where policy.md lives. Only the controller reads it.
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
    let text = std::fs::read_to_string(config_path(home))?;
    Ok(toml::from_str(&text)?)
}

fn keys(home: &std::path::Path) -> sirji::Keystore {
    sirji::Keystore::at(home.join("keys"))
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
    let invite = invite
        .ok_or_else(|| anyhow::anyhow!("--parent <invite> is required"))?;
    let invite = sirji::proto::Invite::decode(invite)?;

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
        root: root.clone(),
    };
    std::fs::write(config_path(&home), toml::to_string_pretty(&config)?)?;

    println!("enrolled as `{name}`");
    println!("home   {}", home.display());
    println!("policy {}", policy::path_in(&root).display());
    Ok(())
}

// ---------------------------------------------------------------------------
// the controller
// ---------------------------------------------------------------------------

async fn controller() -> Result<()> {
    let home = home()?;
    sirji::Settings::load(&home)?.activate();
    let config = load_config(&home)?;

    // Read policy once at startup so a broken file fails loudly here, rather than
    // on the first plea when somebody is waiting.
    let policy = policy::Policy::load(&config.root)?;
    println!("controller `{}` listening as {}", config.name, config.key);
    println!("policy {} ({} bytes)", policy.path.display(), policy.text.len());

    let secret = keys(&home).secret(&id52::decode(&config.key)?)?;
    let endpoint = sirji::bind(secret).await?;

    let listening: Vec<String> = endpoint
        .bound_sockets()
        .iter()
        .map(|a| format!("127.0.0.1:{}", a.port()))
        .collect();
    let registration = tokio::spawn({
        let config = config.clone();
        let home = home.clone();
        async move { register(config, home, listening).await }
    });

    let state = std::sync::Arc::new((config, policy));
    while let Some(incoming) = endpoint.accept().await {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = serve(incoming, state).await {
                eprintln!("connection failed: {e:#}");
            }
        });
    }
    registration.abort();
    Ok(())
}

/// Hold a connection to the parent. While it is up we are resolvable; when it
/// drops we are not.
async fn register(config: Config, home: std::path::PathBuf, listening: Vec<String>) -> Result<()> {
    let store = keys(&home);
    let key = id52::decode(&config.key)?;
    loop {
        let secret = store.secret(&key)?;
        match sirji::daemon::register_device(
            &secret,
            &config.parent,
            &config.parent_hints,
            &listening,
        )
        .await
        {
            Ok(()) => println!("parent connection closed; reconnecting"),
            Err(e) => eprintln!("cannot reach parent: {e:#}"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

async fn serve(
    incoming: sirji::Incoming,
    state: std::sync::Arc<(Config, policy::Policy)>,
) -> Result<()> {
    let (config, policy) = &*state;

    let conn = incoming.await?;
    let (mut send, recv) = conn.accept_bi().await?;
    let mut recv = BufReader::new(recv);

    // The ticket says who this is. We hold no network.toml and could not know
    // otherwise — which is the point: the controller is not an identity service.
    let mut line = String::new();
    recv.read_line(&mut line).await?;
    let knock: Knock = serde_json::from_str(line.trim()).unwrap_or(Knock { ticket: None });

    let asker = match &knock.ticket {
        Some(ticket) => match ticket.verify(&conn.remote_id(), &config.parent) {
            Ok(()) if ticket.name == config.name => {
                ticket.alias.clone().unwrap_or_else(|| "an unnamed peer".into())
            }
            Ok(()) => {
                refuse(&mut send, format!("that ticket is for `{}`", ticket.name)).await?;
                return Ok(());
            }
            Err(e) => {
                refuse(&mut send, format!("{e:#}")).await?;
                return Ok(());
            }
        },
        None => {
            refuse(&mut send, "no ticket — resolve me as `name@org`".into()).await?;
            return Ok(());
        }
    };

    let mut line = String::new();
    recv.read_line(&mut line).await?;
    let nivedana: Nivedana = serde_json::from_str(line.trim())?;
    println!("{asker}: {:?}", nivedana.why);

    let verdict = policy.weigh(&asker, &nivedana);
    match &verdict {
        Verdict::Grant { workers, .. } => println!("  grant, {} worker(s)", workers.len()),
        Verdict::Counter { count, rationale } => println!("  counter {count}: {rationale}"),
        Verdict::Deny { rationale } => println!("  deny: {rationale}"),
    }

    let mut text = serde_json::to_string(&verdict)?;
    text.push('\n');
    send.write_all(text.as_bytes()).await?;
    send.finish()?;
    conn.closed().await;
    Ok(())
}

async fn refuse(send: &mut sirji::SendStream, reason: String) -> Result<()> {
    let mut text = serde_json::to_string(&Verdict::Deny { rationale: reason })?;
    text.push('\n');
    send.write_all(text.as_bytes()).await?;
    send.finish()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// the tester
// ---------------------------------------------------------------------------

async fn test(target: &str, why: &str, args: &[&str]) -> Result<()> {
    let mut nivedana = Nivedana {
        why: why.to_string(),
        count: None,
        class: None,
        role: std::env::var("CM_T_ROLE").ok(),
    };
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--count" => {
                nivedana.count = args.get(i + 1).and_then(|v| v.parse().ok());
                i += 2;
            }
            "--class" => {
                nivedana.class = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            "--role" => {
                nivedana.role = args.get(i + 1).map(|v| (*v).to_string());
                i += 2;
            }
            other => bail!("unrecognised argument: {other}"),
        }
    }

    let home = home()?;
    sirji::Settings::load(&home)?.activate();
    let config = load_config(&home)?;
    let secret = keys(&home).secret(&id52::decode(&config.key)?)?;

    // Ask our own sirji to resolve the controller. We have no network.toml and so
    // cannot know who the organisation is; our parent does.
    let (name, org) = target
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("{target:?} is not name@org"))?;

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

    // Try where the controller said it listens before falling back to discovery.
    // The hints came from the controller itself, via its parent, moments ago.
    let target_key = id52::decode(&device)?;
    let mut conn = None;
    for hint in &hints {
        if let Ok(socket) = hint.parse()
            && let Ok(c) = sirji::endpoint::dial_at(&endpoint, target_key, socket).await
        {
            conn = Some(c);
            break;
        }
    }
    let conn = match conn {
        Some(conn) => conn,
        None => sirji::dial(&endpoint, target_key).await?,
    };
    let (mut send, recv) = conn.open_bi().await?;

    for line in [
        serde_json::to_string(&Knock { ticket: Some(ticket) })?,
        serde_json::to_string(&nivedana)?,
    ] {
        send.write_all(format!("{line}\n").as_bytes()).await?;
    }
    send.finish()?;

    let mut recv = BufReader::new(recv);
    let mut line = String::new();
    recv.read_line(&mut line).await?;
    conn.close(0u32.into(), b"done");
    endpoint.close().await;

    match serde_json::from_str::<Verdict>(line.trim())? {
        Verdict::Grant { workers, rationale } => {
            println!("granted {} worker(s)", workers.len());
            for w in workers {
                println!("  {w}");
            }
            if let Some(r) = rationale {
                println!("({r})");
            }
            Ok(())
        }
        Verdict::Counter { count, rationale } => {
            println!("countered: {count} — {rationale}");
            Ok(())
        }
        Verdict::Deny { rationale } => bail!("denied: {rationale}"),
    }
}

/// The first line on any cm connection.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Knock {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ticket: Option<sirji::Ticket>,
}
