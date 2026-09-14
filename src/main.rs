mod modes;
mod relay;
mod server;
mod term;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::io::{ErrorKind, IsTerminal, Read, Write};
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

    /// File holding the token
    #[arg(long, env = "TCOMP_TOKEN_FILE", global = true)]
    token_file: Option<String>,

    /// Let the web view type into this terminal
    #[arg(long, env = "TCOMP_ALLOW_INPUT", global = true)]
    allow_input: bool,

    /// Exit when the command exits instead of offering to restart it
    #[arg(long, env = "TCOMP_EXIT_ON_END", global = true)]
    exit_on_end: bool,

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

        /// Exit when the command exits instead of offering to restart it
        #[arg(long, env = "TCOMP_EXIT_ON_END")]
        exit_on_end: bool,

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
    Metadata(modes::Seen),
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
            exit_on_end,
            bind,
            public_url,
            cmd,
        }) => {
            let token = server::config::resolve_token(token, token_file)?;
            let mode = PtyMode::Standalone {
                bind: listen_addr(bind),
                public_url: server::config::normalize_public_url(&public_url.unwrap_or_default()),
            };
            let code =
                runtime.block_on(run_pty(cmd, mode, name, token, allow_input, exit_on_end))?;
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
                cli.exit_on_end,
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

/// How long the restart prompt waits before giving up and exiting.
const RESTART_PROMPT: Duration = Duration::from_secs(5);

struct Shell {
    master: Box<dyn MasterPty + Send>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
}

fn spawn_shell(
    command: &[String],
    cwd: Option<&std::path::Path>,
    cols: u16,
    rows: u16,
) -> Result<Shell> {
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
    if let Some(cwd) = cwd {
        builder.cwd(cwd);
    }
    builder.env("TCOMP", "1");

    let child = pair
        .slave
        .spawn_command(builder)
        .with_context(|| format!("failed to spawn {}", command[0]))?;
    drop(pair.slave);

    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    Ok(Shell {
        master: pair.master,
        reader,
        writer,
        child,
    })
}

/// Where keystrokes go: the live shell, or the restart prompt between shells.
enum Sink {
    Pty(Box<dyn Write + Send>),
    Prompt(UnboundedSender<Vec<u8>>),
    Closed,
}

struct Input(std::sync::Mutex<Sink>);

impl Input {
    fn new(writer: Box<dyn Write + Send>) -> Self {
        Input(std::sync::Mutex::new(Sink::Pty(writer)))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Sink> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn attach(&self, writer: Box<dyn Write + Send>) {
        *self.lock() = Sink::Pty(writer);
    }

    fn keys(&self) -> tokio::sync::mpsc::UnboundedReceiver<Vec<u8>> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *self.lock() = Sink::Prompt(tx);
        rx
    }

    fn deliver(&self, chunk: Vec<u8>) {
        let mut sink = self.lock();
        match &mut *sink {
            Sink::Pty(writer) => {
                if writer.write_all(&chunk).is_err() {
                    *sink = Sink::Closed;
                    return;
                }
                let _ = writer.flush();
            }
            Sink::Prompt(keys) => {
                let _ = keys.send(chunk);
            }
            Sink::Closed => {}
        }
    }
}

async fn run_pty(
    command: Vec<String>,
    mode: PtyMode,
    name: Option<String>,
    token: Option<String>,
    allow_input: bool,
    exit_on_end: bool,
) -> Result<i32> {
    let (cols, rows) = term::size();
    let cwd = std::env::current_dir().ok();

    let shell = spawn_shell(&command, cwd.as_deref(), cols, rows)?;

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
                cwd: cwd.as_ref().map(|path| path.display().to_string()),
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

    tokio::spawn(async {
        tokio::time::sleep(term::NOTE_HOLD).await;
        let _ = term::give_up_waiting();
    });

    let input = std::sync::Arc::new(Input::new(shell.writer));
    let stdin_tx = writes_tx.clone();
    std::thread::spawn({
        let input = input.clone();
        move || pump_writes(&input, writes_rx)
    });
    std::thread::spawn(move || pump_stdin(stdin_tx));

    let mut master = shell.master;
    let mut killer = shell.child.clone_killer();
    let mut exit_rx = wait_for(shell.child);
    let mut output_done = Some(std::thread::spawn({
        let relay_tx = relay_tx.clone();
        move || pump_output(shell.reader, relay_tx)
    }));

    let mut winch = signal(SignalKind::window_change())?;
    let mut term_sig = signal(SignalKind::terminate())?;
    let mut hup_sig = signal(SignalKind::hangup())?;

    let code = 'session: loop {
        let code = loop {
            tokio::select! {
                _ = winch.recv() => {
                    let (c, r) = term::size();
                    let _ = master.resize(PtySize { rows: r, cols: c, pixel_width: 0, pixel_height: 0 });
                    let _ = events_tx.send(Event::Resize { cols: c, rows: r });
                }
                _ = term_sig.recv() => {
                    let _ = killer.kill();
                    break 'session 143;
                }
                _ = hup_sig.recv() => {
                    let _ = killer.kill();
                    break 'session 129;
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

        if let Some(handle) = output_done.take() {
            drain_output(handle).await;
        }
        term::release_output();

        if exit_on_end || !std::io::stdin().is_terminal() {
            break code;
        }

        announce(
            &relay_tx,
            &format!(
                "{} exited ({code}) — press r to restart, anything else quits (5s)",
                program(&command[0])
            ),
        );
        let mut keys = input.keys();
        let deadline = tokio::time::Instant::now() + RESTART_PROMPT;
        'ask: loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break 'session code,
                _ = winch.recv() => {
                    let (c, r) = term::size();
                    let _ = events_tx.send(Event::Resize { cols: c, rows: r });
                }
                _ = term_sig.recv() => break 'session 143,
                _ = hup_sig.recv() => break 'session 129,
                chunk = keys.recv() => {
                    let Some(chunk) = chunk else { break 'session code };
                    match chunk.as_slice() {
                        [b'r' | b'R'] => break 'ask,
                        [] => {}
                        _ => break 'session code,
                    }
                }
            }
        }

        let (c, r) = term::size();
        let shell = match spawn_shell(&command, cwd.as_deref(), c, r) {
            Ok(shell) => shell,
            Err(error) => {
                term::note(term::Note::Warn, &format!("restart failed: {error}"));
                break code;
            }
        };

        let reset = modes::cleanup();
        if !reset.is_empty() {
            let _ = term::child_output(&reset);
            if let Some(tx) = &relay_tx {
                let _ = tx.send(Event::Output(reset));
            }
        }

        input.attach(shell.writer);
        master = shell.master;
        killer = shell.child.clone_killer();
        exit_rx = wait_for(shell.child);
        output_done = Some(std::thread::spawn({
            let relay_tx = relay_tx.clone();
            move || pump_output(shell.reader, relay_tx)
        }));
        let _ = events_tx.send(Event::Resize { cols: c, rows: r });
    };

    if let Some(handle) = output_done.take() {
        drain_output(handle).await;
    }

    let _ = events_tx.send(Event::Exit { code });
    drop(events_tx);
    let _ = tokio::time::timeout(Duration::from_secs(3), relay_task).await;

    Ok(code)
}

fn program(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

fn wait_for(
    mut child: Box<dyn Child + Send + Sync>,
) -> tokio::sync::oneshot::Receiver<std::io::Result<portable_pty::ExitStatus>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait());
    });
    rx
}

async fn drain_output(handle: std::thread::JoinHandle<()>) {
    let _ = tokio::time::timeout(
        Duration::from_millis(500),
        tokio::task::spawn_blocking(move || {
            let _ = handle.join();
        }),
    )
    .await;
}

/// Say the same thing on the local terminal and in every viewer.
fn announce(relay: &Option<UnboundedSender<Event>>, body: &str) {
    term::note(term::Note::Warn, body);
    if let Some(tx) = relay {
        let line = term::styled_line(term::Note::Warn, body);
        let _ = tx.send(Event::Output(line.into_bytes()));
    }
}

fn pump_output(mut reader: Box<dyn Read + Send>, relay: Option<UnboundedSender<Event>>) {
    let mut buf = [0u8; 16384];
    let mut pending = Vec::new();
    let mut reported = modes::Seen::default();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &buf[..n];
                let seen = modes::observe(&mut pending, chunk);
                if term::child_output(chunk).is_err() {
                    break;
                }
                if let Some(tx) = &relay {
                    if tx.send(Event::Output(chunk.to_vec())).is_err() {
                        break;
                    }
                    let changed = changes(&mut reported, seen);
                    if changed != modes::Seen::default()
                        && tx.send(Event::Metadata(changed)).is_err()
                    {
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

fn pump_writes(input: &Input, writes: std::sync::mpsc::Receiver<Vec<u8>>) {
    while let Ok(chunk) = writes.recv() {
        input.deliver(chunk);
    }
}

fn new_token() -> String {
    format!("{:032x}", rand::random::<u128>())
}

fn changes(reported: &mut modes::Seen, seen: modes::Seen) -> modes::Seen {
    let mut changed = modes::Seen::default();
    if seen.title.is_some() && seen.title != reported.title {
        reported.title.clone_from(&seen.title);
        changed.title = seen.title;
    }
    if seen.cwd.is_some() && seen.cwd != reported.cwd {
        reported.cwd.clone_from(&seen.cwd);
        changed.cwd = seen.cwd;
    }
    changed
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
    use super::{listen_addr, program};

    #[test]
    fn the_restart_prompt_names_the_command_not_its_path() {
        assert_eq!(program("/usr/bin/bash"), "bash");
        assert_eq!(program("bash"), "bash");
        assert_eq!(program(""), "");
    }

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
