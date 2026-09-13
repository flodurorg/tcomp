mod modes;
mod relay;
mod term;

use anyhow::{Context, Result};
use clap::Parser;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{ErrorKind, Read, Write};
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Parser, Debug)]
#[command(
    name = "tcomp",
    about = "terminal companion — run a command and broadcast a read-only view"
)]
struct Args {
    /// Relay base URL, e.g. https://tcomp.example.com
    #[arg(long, env = "TCOMP_RELAY")]
    relay: Option<String>,

    /// Label shown in the web UI (defaults to hostname)
    #[arg(long, env = "TCOMP_NAME")]
    name: Option<String>,

    /// Shared secret presented to the relay
    #[arg(long, env = "TCOMP_TOKEN")]
    token: Option<String>,

    /// Run the command without connecting to a relay
    #[arg(long)]
    local: bool,

    /// Let the web view type into this terminal
    #[arg(long)]
    allow_input: bool,

    #[arg(last = true, required = true)]
    command: Vec<String>,
}

pub enum Event {
    Output(Vec<u8>),
    Resize { cols: u16, rows: u16 },
    Exit { code: i32 },
}

fn main() -> Result<()> {
    let args = Args::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let code = runtime.block_on(run(args))?;
    term::restore();
    std::process::exit(code);
}

async fn run(args: Args) -> Result<i32> {
    let (cols, rows) = term::size();

    let pair = native_pty_system().openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut builder = CommandBuilder::new(&args.command[0]);
    for arg in &args.command[1..] {
        builder.arg(arg);
    }
    if let Ok(cwd) = std::env::current_dir() {
        builder.cwd(cwd);
    }
    builder.env("TCOMP", "1");

    let mut child = pair
        .slave
        .spawn_command(builder)
        .with_context(|| format!("failed to spawn {}", args.command[0]))?;
    drop(pair.slave);

    let master = pair.master;
    let reader = master.try_clone_reader()?;
    let writer = master.take_writer()?;

    let _raw = term::RawGuard::new()?;

    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let (writes_tx, writes_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let relay_url = if args.local { None } else { args.relay.clone() };
    let relay_tx = relay_url.as_ref().map(|_| events_tx.clone());

    let relay_task = match relay_url {
        Some(url) => {
            let params = relay::Params {
                relay: url,
                name: args.name.clone().unwrap_or_else(hostname),
                cmd: args.command.join(" "),
                token: args.token.clone(),
                cols,
                rows,
                input: args.allow_input,
            };
            let writes = args.allow_input.then_some(writes_tx.clone());
            tokio::spawn(relay::run(params, events_rx, writes))
        }
        None => tokio::spawn(async move {
            let mut events_rx = events_rx;
            while events_rx.recv().await.is_some() {}
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
                let (cols, rows) = term::size();
                let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
                let _ = events_tx.send(Event::Resize { cols, rows });
            }
            _ = term_sig.recv() => {
                let mut killer = killer;
                let _ = killer.kill();
                break 143;
            }
            _ = hup_sig.recv() => {
                let mut killer = killer;
                let _ = killer.kill();
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

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}
