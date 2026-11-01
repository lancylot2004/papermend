use anstream::{eprint, eprintln, print, println};
use clap::{Parser, Subcommand};
use papermend::api::model::{ClientInfo, LoggedInUser, Machine, Os, PrinterDef};
use papermend::api::{self, Client};
use papermend::cups;
use papermend::error::{Error, Result};
use papermend::proxy::{self, Tunnel};
use papermend::reconcile;
use papermend::style::{self, BAD, CLAP_STYLE, GOOD, KEY, WARN};
use std::collections::HashMap;
use std::net::IpAddr;
use std::process::ExitCode;
use std::time::Duration;
use url::Url;

#[derive(Debug, Parser)]
#[command(
    name = "papermend",
    version = env!("CARGO_PKG_VERSION"),
    about = "An open-source PaperCut Print Deploy subtitute with no long-running background tasks.",
    styles = CLAP_STYLE,
)]
struct Cli {
    /// Print Deploy server base URL.
    #[arg(
        long,
        env = "PAPERMEND_SERVER",
        global = true,
        default_value = "https://icl-print-service.ic.ac.uk:9174"
    )]
    server: Url,

    /// PaperCut username for `sync`. Asked for if omitted.
    #[arg(short, long, env = "PAPERMEND_USERNAME", global = true)]
    username: Option<String>,

    /// Per-request HTTP timeout in seconds.
    #[arg(long, env = "PAPERMEND_TIMEOUT", global = true, default_value_t = 30)]
    timeout: u64,

    /// SSH proxies to fall back to when the server is not directly reachable: `~/.ssh/config`
    /// aliases or `user@host`, tried in random order. Passwordless (key) SSH is required.
    #[arg(long, env = "PAPERMEND_PROXY", global = true, value_delimiter = ',', value_name = "SSH")]
    proxy: Vec<String>,

    /// Seconds to probe the direct connection before falling back to the proxies.
    #[arg(long, env = "PAPERMEND_PROBE_TIMEOUT", global = true, default_value_t = 2)]
    probe_timeout: u64,

    /// Print every server response, with tokens redacted, to stderr.
    #[arg(short, long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show the server, whether it is reachable, and the queues papermend manages.
    Status,

    /// Log in, then install, update, and remove queues to match the server. Stores nothing.
    Sync {
        /// Print the plan without changing anything.
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Install these Mobility Print queues instead of asking the Print Deploy server which
        /// queues to have. For when `/deploy/printers` offers nothing, e.g. unzoned networks.
        #[arg(long = "printer", value_name = "NAME")]
        printers: Vec<String>,

        /// Mobility Print server that `--printer` queues live on.
        #[arg(
            long,
            env = "PAPERMEND_MOBILITY",
            default_value = "https://icl-print-mobile.ic.ac.uk:9164"
        )]
        mobility: Url,
    },

    /// Remove every queue papermend installed.
    Remove {
        /// Print what would be removed without changing anything.
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    // reqwest is built with `rustls-no-provider` (see Cargo.toml), so the binary picks the TLS
    // crypto provider: ring, which cross-compiles cleanly to every release target.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("no other TLS provider is installed before this");
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{BAD}error:{BAD:#} {}", e.report());
            ExitCode::FAILURE
        }
    }
}

/// Dispatches a parsed command.
async fn run(cli: Cli) -> Result<()> {
    let net = Net {
        server: cli.server,
        timeout: Duration::from_secs(cli.timeout),
        probe: Duration::from_secs(cli.probe_timeout),
        proxies: cli.proxy,
        verbose: cli.verbose,
    };
    match cli.command {
        Command::Status => status(&net).await,
        Command::Sync { dry_run, printers, mobility } => {
            let username = username(cli.username)?;
            sync(&net, &username, &printers, &mobility, dry_run).await
        }
        Command::Remove { dry_run } => remove(dry_run).await,
    }
}

/// How to reach the Print Deploy server, resolved once from the global flags.
struct Net {
    server: Url,
    timeout: Duration,
    /// Direct-connection probe timeout before proxy fallback.
    probe: Duration,
    /// SSH destinations to tunnel through, in the user's order.
    proxies: Vec<String>,
    verbose: bool,
}

impl Net {
    /// A client for the Print Deploy server, optionally through a local SOCKS port.
    fn client(&self, socks: Option<u16>) -> Result<Client> {
        Client::new(self.server.clone(), self.timeout, self.verbose, socks)
    }

    /// A short-timeout client for reachability probes, which never prints.
    fn prober(&self, socks: Option<u16>) -> Result<Client> {
        Client::new(self.server.clone(), self.probe, false, socks)
    }
}

/// Opens a SOCKS tunnel through the first proxy that comes up and can reach the server.
async fn through_proxies(net: &Net) -> Result<Tunnel> {
    for destination in proxy::shuffled(&net.proxies) {
        match Tunnel::open(destination).await {
            Ok(tunnel) => match net.prober(Some(tunnel.port))?.ping().await {
                Ok(()) => return Ok(tunnel),
                Err(e) => println!(
                    "{}",
                    style::line(
                        WARN,
                        "warn",
                        format!("server unreachable via {destination} ({})", e.report())
                    )
                ),
            },
            Err(e) => println!("{}", style::line(WARN, "warn", e.report())),
        }
    }
    Err(Error::Invalid("The server is unreachable directly and through every proxy.".into()))
}

/// Probes the direct route; when it fails and proxies are configured, falls back to a tunnel.
/// `None` means connect directly.
async fn connect(net: &Net) -> Result<Option<Tunnel>> {
    if net.prober(None)?.ping().await.is_ok() || net.proxies.is_empty() {
        return Ok(None);
    }
    through_proxies(net).await.map(Some)
}

/// Prints server reachability, directly and through the proxies, and managed queues.
async fn status(net: &Net) -> Result<()> {
    println!("{}", style::line(KEY, "server", &net.server));
    let mut reachable = match net.prober(None)?.ping().await {
        Ok(()) => format!("direct {GOOD}yes{GOOD:#}"),
        Err(e) => format!("direct {BAD}no{BAD:#} ({})", e.report()),
    };
    if !net.proxies.is_empty() {
        reachable += &match through_proxies(net).await {
            Ok(tunnel) => format!(" · via {} {GOOD}yes{GOOD:#}", tunnel.via),
            Err(e) => format!(" · via proxy {BAD}no{BAD:#} ({})", e.report()),
        };
    }
    println!("{}", style::line(KEY, "reachable", reachable));

    let managed = cups::managed().await?;
    println!("{}", style::line(KEY, "queues", managed.len()));
    for queue in managed {
        println!("  {} [{}]", queue.name, api::redact(&queue.uri));
    }
    Ok(())
}

/// Who and what is asking, for `/deploy/printers`.
async fn client_info(username: &str) -> Result<ClientInfo> {
    Ok(ClientInfo {
        version: CLIENT_VERSION.into(),
        logged_in_user: LoggedInUser { username: username.to_owned(), kind: String::new() },
        machine: Machine {
            hostname: String::new(),
            os: os_info().await?,
            ip_addresses: ip_addresses()?,
            active_directory_domain_name: String::new(),
        },
    })
}

/// Logs in, fetches the server's queues and each Mobility Print queue's per-user address, and makes
/// CUPS match. Nothing is stored: once a queue exists, CUPS holds its address.
async fn sync(
    net: &Net,
    username: &str,
    printers: &[String],
    mobility: &Url,
    dry_run: bool,
) -> Result<()> {
    let password = rpassword::prompt_password(format!("Password for {username}: "))?;
    // The tunnel, if any, lives for the whole sync; dropping it at the end kills the ssh child.
    let tunnel = connect(net).await?;
    let socks = tunnel.as_ref().map(|tunnel| tunnel.port);
    if let Some(tunnel) = &tunnel {
        println!("{}", style::line(KEY, "proxy", &tunnel.via));
    }
    // `--printer` queues skip Print Deploy entirely: the Mobility Print lookup authenticates on
    // its own, so no login or printer list is needed.
    let desired = if printers.is_empty() {
        let mut client = net.client(socks)?;
        client.login(username, &password).await?;
        client.printers(&client_info(username).await?).await?
    } else {
        printers.iter().map(|name| PrinterDef::mobility(name, mobility)).collect()
    };
    let installed = cups::managed().await?;
    if desired.is_empty() {
        // An empty list is more likely a response we failed to parse than a real "no printers",
        // and acting on it would delete every managed queue. Use `remove` to clear them on purpose.
        println!(
            "{}",
            style::line(
                WARN,
                "warn",
                "the server returned no printers; changing nothing. Re-run with -v."
            )
        );
        return Ok(());
    }
    let mut addresses = HashMap::new();
    for printer in &desired {
        let Some(mobility) = reconcile::mobility_server(printer) else { continue };
        let mobility = Client::new(mobility, net.timeout, net.verbose, socks)?;
        match mobility.mobility_printer_url(&printer.name, username, &password).await {
            Ok(address) => {
                addresses.insert(printer.name.clone(), address);
            }
            Err(e) => println!(
                "{}",
                style::line(
                    WARN,
                    "warn",
                    format!("{}: address lookup failed ({})", printer.name, e.report())
                )
            ),
        }
    }
    let plan = reconcile::plan(&desired, &installed, &addresses);

    print!("{plan}");
    if dry_run {
        return Ok(());
    }

    for queue in &plan.add {
        cups::add(queue).await?;
    }
    for name in &plan.remove {
        cups::remove(name).await?;
    }
    Ok(())
}

/// Removes every managed queue.
async fn remove(dry_run: bool) -> Result<()> {
    for queue in cups::managed().await? {
        println!(
            "{}",
            style::line(BAD, "del", format!("{} [{}]", queue.name, api::redact(&queue.uri)))
        );
        if !dry_run {
            cups::remove(&queue.name).await?;
        }
    }
    Ok(())
}

/// Version reported to the server: the official client release whose wire format papermend
/// follows, so the server treats requests the same way.
const CLIENT_VERSION: &str = "2025-07-01-1603";

/// This machine's OS, named as the official client names it (Go's `runtime.GOOS`).
#[cfg(target_os = "macos")]
async fn os_info() -> Result<Os> {
    let output = tokio::process::Command::new("sw_vers").arg("-productVersion").output().await?;
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok(Os { name: "darwin".into(), os_name: "macOS".into(), version, arch: go_arch().into() })
}

/// This machine's OS, named as the official client names it (Go's `runtime.GOOS`).
#[cfg(not(target_os = "macos"))]
async fn os_info() -> Result<Os> {
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    Ok(Os {
        name: std::env::consts::OS.into(),
        os_name: key_value(&release, "NAME").unwrap_or("Linux").into(),
        version: key_value(&release, "VERSION_ID").unwrap_or_default().into(),
        arch: go_arch().into(),
    })
}

/// Reads one field from `KEY=value` text such as `/etc/os-release`, without surrounding quotes.
#[cfg(not(target_os = "macos"))]
fn key_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .filter_map(|line| line.trim().split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, value)| value.trim().trim_matches('"'))
}

/// This machine's architecture as Go's `runtime.GOARCH` spells it.
fn go_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        other => other,
    }
}

/// The PaperCut username from `--username`, or asked for. There is deliberately no default: the
/// OS account name is usually not the PaperCut one, and a wrong name fails quietly.
fn username(flag: Option<String>) -> Result<String> {
    if let Some(username) = flag {
        return Ok(username);
    }
    eprint!("PaperCut username: ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    match line.trim() {
        "" => Err(Error::Invalid("A PaperCut username is required.".into())),
        username => Ok(username.to_owned()),
    }
}

/// Non-loopback local IPv4 addresses. The server rejects the whole request if any address is
/// IPv6 ("is not a valid IPv4 address"), and its zones are IPv4 ranges.
fn ip_addresses() -> Result<Vec<String>> {
    Ok(if_addrs::get_if_addrs()?
        .into_iter()
        .filter_map(|interface| match interface.ip() {
            // 192.0.0.0/29 is macOS's NAT64 (464XLAT) address on IPv6-only networks.
            IpAddr::V4(ip) if !ip.is_loopback() && ip.octets()[..3] != [192, 0, 0] => Some(ip),
            _ => None,
        })
        .map(|ip| ip.to_string())
        .collect())
}
