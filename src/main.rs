mod modes;
mod relay;
mod server;
mod term;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{ErrorKind, Read, Write};
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Parser, Debug)]
#[command(
    name = "tcomp",
    about = "terminal companion — share a terminal session in the browser",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,

    // Bare `tcomp [opts] -- <cmd>` → remote relay mode
    /// Relay base URL, e.g. https://tcomp.example.com
    #[arg(long, env = "TCOMP_RELAY")]
    relay: Option<String>,

    #[arg(long, env = "TCOMP_NAME", global = true)]
    name: Option<String>,

    /// Token to present to the relay
    #[arg(long, env = "TCOMP_TOKEN", global = true)]
    token: Option<String>,

    /// File holding the token, read at startup so the secret stays off the
    /// command line and out of the environment
    #[arg(long, env = "TCOMP_TOKEN_FILE", global = true)]
    token_file: Option<String>,

    /// Let the web view type into this terminal
    #[arg(long, env = "TCOMP_ALLOW_INPUT", global = true)]
    allow_input: bool,

    #[arg(last = true)]
    cmd: Vec<String>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the relay server (used by Docker)
    Serve,

    /// Start an embedded relay and run a command, printing the watch URL
    Standalone {
        #[arg(long, env = "TCOMP_NAME")]
        name: Option<String>,

        /// Token the embedded relay requires; one is minted when unset
        #[arg(long, env = "TCOMP_TOKEN")]
        token: Option<String>,

        /// File holding the token the embedded relay requires
        #[arg(long, env = "TCOMP_TOKEN_FILE")]
        token_file: Option<String>,

        /// Let the web view type into this terminal
        #[arg(long, env = "TCOMP_ALLOW_INPUT")]
        allow_input: bool,

        /// Address the embedded relay listens on; a bare IP gets a random port
        #[arg(long, env = "TCOMP_BIND")]
        bind: Option<String>,

        /// Base URL to build watch links from (default: the bound address)
        #[arg(long, env = "TCOMP_PUBLIC_URL")]
        public_url: Option<String>,

        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
}

pub enum Event {
    Output(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Exit { code: i32 },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // tokio-tungstenite leaves the rustls provider to the application, and
    // without one connect_async panics on the first https relay.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    match cli.command {
        Some(Cmd::Serve) => {
            runtime.block_on(run_serve())?;
            Ok(())
        }
        Some(Cmd::Standalone {
            name,
            token,
            token_file,
            allow_input,
            bind,
            public_url,
            cmd,
        }) => {
            let token = server::config::resolve_token(token, token_file)?;
            let mode = PtyMode::Standalone {
                bind: listen_addr(bind),
                public_url: server::config::normalize_public_url(&public_url.unwrap_or_default()),
            };
            let code = runtime.block_on(run_pty(cmd, mode, name, token, allow_input))?;
            term::restore();
            std::process::exit(code);
        }
        None => {
            if cli.cmd.is_empty() {
                eprintln!("usage: tcomp [--relay URL] -- <command> [args...]");
                eprintln!("       tcomp standalone -- <command> [args...]");
                eprintln!("       tcomp serve");
                std::process::exit(1);
            }
            let relay = cli
                .relay
                .or_else(|| std::env::var("TCOMP_RELAY").ok().filter(|s| !s.is_empty()));
            let Some(relay) = relay else {
                eprintln!("tcomp: --relay or TCOMP_RELAY required (or use `tcomp standalone`)");
                std::process::exit(1);
            };
            let token = server::config::resolve_token(cli.token, cli.token_file)?;
            let code = runtime.block_on(run_pty(
                cli.cmd,
                PtyMode::Remote { relay },
                cli.name,
                token,
                cli.allow_input,
            ))?;
            term::restore();
            std::process::exit(code);
        }
    }
}

enum PtyMode {
    /// Spin up an embedded relay on `bind`, print the URL, then connect.
    Standalone { bind: String, public_url: String },
    /// Connect to an externally-running relay.
    Remote { relay: String },
}

async fn run_serve() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "tcomp=info,tower_http=warn".into()),
        )
        .init();

    let config = server::config::Config::from_env()?;
    server::serve(config, |addr| {
        tracing::info!(bind = %addr, "tcomp relay ready");
    })
    .await
}

async fn run_pty(
    command: Vec<String>,
    mode: PtyMode,
    name: Option<String>,
    token: Option<String>,
    allow_input: bool,
) -> Result<i32> {
    let (cols, rows) = term::size();

    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut builder = CommandBuilder::new(&command[0]);
    for arg in &command[1..] {
        builder.arg(arg);
    }
    if let Ok(cwd) = std::env::current_dir() {
        builder.cwd(cwd);
    }
    builder.env("TCOMP", "1");

    let mut child = pair
        .slave
        .spawn_command(builder)
        .with_context(|| format!("failed to spawn {}", command[0]))?;
    drop(pair.slave);

    let master = pair.master;
    let reader = master.try_clone_reader()?;
    let writer = master.take_writer()?;

    let _raw = term::RawGuard::new()?;

    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let (writes_tx, writes_rx) = std::sync::mpsc::channel::<Vec<u8>>();

    // Resolve the relay URL — for Standalone, start an embedded server first.
    let mut token = token;
    let relay_url: Option<String> = match mode {
        PtyMode::Remote { relay } => Some(relay),
        PtyMode::Standalone { bind, public_url } => {
            // Only a minted token is safe to put in the printed link.
            let minted = token.is_none();
            if minted {
                token = Some(new_token());
            }
            // Bind on a random port, start relay in background.
            let cfg = server::config::Config {
                bind,
                public_url,
                web_dir: web_dir(),
                token: token.clone(),
                token_in_links: minted,
                ..server::config::Config::from_env()?
            };
            // Channel so the server can hand us the bound address.
            let (addr_tx, addr_rx) = tokio::sync::oneshot::channel::<String>();
            let mut addr_tx = Some(addr_tx);
            // If the relay cannot bind it reports the reason here; on_ready never
            // fires, addr_tx drops, and the await below fails rather than hanging.
            let (fail_tx, fail_rx) = tokio::sync::oneshot::channel::<anyhow::Error>();
            tokio::spawn(async move {
                if let Err(error) = server::serve(cfg, move |addr| {
                    if let Some(tx) = addr_tx.take() {
                        let _ = tx.send(addr.to_string());
                    }
                })
                .await
                {
                    let _ = fail_tx.send(error);
                }
            });
            let addr = match addr_rx.await {
                Ok(addr) => addr,
                Err(_) => {
                    return Err(match fail_rx.await {
                        Ok(error) => error.context("embedded relay failed to start"),
                        Err(_) => anyhow::anyhow!("embedded relay failed to start"),
                    })
                }
            };
            Some(format!("http://{addr}"))
        }
    };

    let relay_tx = relay_url.as_ref().map(|_| events_tx.clone());

    let relay_task = match relay_url {
        Some(url) => {
            let params = relay::Params {
                relay: url,
                name: name.unwrap_or_else(hostname),
                cmd: command.join(" "),
                token,
                cols,
                rows,
                input: allow_input,
            };
            let writes = allow_input.then_some(writes_tx.clone());
            tokio::spawn(relay::run(params, events_rx, writes))
        }
        None => tokio::spawn(async move {
            let mut rx = events_rx;
            while rx.recv().await.is_some() {}
        }),
    };

    let stdin_tx = writes_tx.clone();
    let output_done = std::thread::spawn(move || pump_output(reader, relay_tx));
    std::thread::spawn(move || pump_writes(writer, writes_rx));
    std::thread::spawn(move || pump_stdin(stdin_tx));

    let (exit_tx, mut exit_rx) = tokio::sync::oneshot::channel();
    let killer = child.clone_killer();
    std::thread::spawn(move || {
        let _ = exit_tx.send(child.wait());
    });

    let mut winch = signal(SignalKind::window_change())?;
    let mut term_sig = signal(SignalKind::terminate())?;
    let mut hup_sig = signal(SignalKind::hangup())?;

    let code = loop {
        tokio::select! {
            _ = winch.recv() => {
                let (c, r) = term::size();
                let _ = master.resize(PtySize { rows: r, cols: c, pixel_width: 0, pixel_height: 0 });
                let _ = events_tx.send(Event::Resize { cols: c, rows: r });
            }
            _ = term_sig.recv() => {
                let mut k = killer;
                let _ = k.kill();
                break 143;
            }
            _ = hup_sig.recv() => {
                let mut k = killer;
                let _ = k.kill();
                break 129;
            }
            status = &mut exit_rx => {
                break status
                    .ok()
                    .and_then(|s| s.ok())
                    .map(|s| s.exit_code() as i32)
                    .unwrap_or(1);
            }
        }
    };

    let _ = tokio::time::timeout(
        Duration::from_millis(500),
        tokio::task::spawn_blocking(move || {
            let _ = output_done.join();
        }),
    )
    .await;

    let _ = events_tx.send(Event::Exit { code });
    drop(events_tx);
    let _ = tokio::time::timeout(Duration::from_secs(3), relay_task).await;

    Ok(code)
}

fn pump_output(mut reader: Box<dyn Read + Send>, relay: Option<UnboundedSender<Event>>) {
    let mut stdout = std::io::stdout();
    let mut buf = [0u8; 16384];
    let mut pending = Vec::new();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &buf[..n];
                modes::observe(&mut pending, chunk);
                if stdout.write_all(chunk).is_err() {
                    break;
                }
                let _ = stdout.flush();
                if let Some(tx) = &relay {
                    if tx.send(Event::Output(chunk.to_vec())).is_err() {
                        break;
                    }
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

fn pump_stdin(writes: std::sync::mpsc::Sender<Vec<u8>>) {
    let mut stdin = std::io::stdin();
    let mut buf = [0u8; 4096];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if writes.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

fn pump_writes(mut writer: Box<dyn Write + Send>, writes: std::sync::mpsc::Receiver<Vec<u8>>) {
    while let Ok(chunk) = writes.recv() {
        if writer.write_all(&chunk).is_err() {
            break;
        }
        let _ = writer.flush();
    }
}

fn new_token() -> String {
    format!("{:032x}", rand::random::<u128>())
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn listen_addr(bind: Option<String>) -> String {
    let Some(bind) = bind.map(|b| b.trim().to_string()).filter(|b| !b.is_empty()) else {
        return "127.0.0.1:0".into();
    };
    match bind.parse::<std::net::IpAddr>() {
        Ok(ip) => std::net::SocketAddr::new(ip, 0).to_string(),
        Err(_) => bind,
    }
}

/// Locate the web directory relative to the binary or cwd.
fn web_dir() -> String {
    // Running from the repo root (dev): use ./web
    if std::path::Path::new("web/index.html").exists() {
        return "web".into();
    }
    // Installed alongside the binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("web");
            if candidate.join("index.html").exists() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    // Fallback: let the env default handle it
    server::config::web_dir_from_env()
}

#[cfg(test)]
mod tests {
    use super::listen_addr;

    #[test]
    fn defaults_to_loopback_on_a_random_port() {
        assert_eq!(listen_addr(None), "127.0.0.1:0");
        assert_eq!(listen_addr(Some("   ".into())), "127.0.0.1:0");
    }

    #[test]
    fn bare_ip_gets_a_random_port() {
        assert_eq!(
            listen_addr(Some("100.101.102.103".into())),
            "100.101.102.103:0"
        );
        assert_eq!(listen_addr(Some("fd7a::1".into())), "[fd7a::1]:0");
    }

    #[test]
    fn explicit_port_is_kept() {
        assert_eq!(
            listen_addr(Some("100.101.102.103:8080".into())),
            "100.101.102.103:8080"
        );
        assert_eq!(listen_addr(Some("[fd7a::1]:8080".into())), "[fd7a::1]:8080");
    }
}
