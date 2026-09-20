//! Drives real `tcomp` client processes and a real (non-headless, run under
//! Xvfb — see `chromedriver_capabilities`) Chromium to exercise the dashboard
//! end to end: 2-3 sessions show up as floating windows, one gets selected,
//! and keystrokes round-trip in both directions (client -> browser, browser
//! -> client). Needs `chromedriver`, `chromium` and `xvfb-run` on PATH — all
//! three are in the flake devShell. Ignored by default; run explicitly with:
//!   cargo test --test floating_windows -- --ignored

use fantoccini::actions::{InputSource, KeyAction, KeyActions};
use fantoccini::key::Key;
use fantoccini::{ClientBuilder, Locator};
use serde_json::{Map, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BOOT_TIMEOUT: Duration = Duration::from_secs(10);
const UI_TIMEOUT: Duration = Duration::from_secs(15);

/// A spawned process, killed (along with any children it spawned itself —
/// `chromedriver` under `xvfb-run` in particular) as soon as this drops, even
/// if that happens by unwinding out of a panic mid-setup.
struct Guarded(Child);

impl Guarded {
    /// Spawns `command` in its own process group so [`Guarded::drop`] can
    /// kill the whole tree, not just this one pid.
    fn spawn(command: &mut Command) -> Self {
        Guarded(command.process_group(0).spawn().expect("spawn process"))
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{}", self.0.id())])
            .status();
        let _ = self.0.wait();
    }
}

struct ClientProcess {
    #[allow(dead_code)] // kept alive for its Drop, which kills the process
    guard: Guarded,
    stdin: ChildStdin,
    stdout: Arc<Mutex<Vec<u8>>>,
    id: String,
    key: Option<String>,
}

impl ClientProcess {
    fn type_line(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("write to client stdin");
    }

    fn stdout_contains(&self, needle: &str) -> bool {
        String::from_utf8_lossy(&self.stdout.lock().unwrap()).contains(needle)
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .unwrap()
        .port()
}

fn collect_into(mut read: impl Read + Send + 'static) -> Arc<Mutex<Vec<u8>>> {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let handle = buf.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match read.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => handle.lock().unwrap().extend_from_slice(&chunk[..n]),
            }
        }
    });
    buf
}

/// Parses `tcomp: watch at http://host:port/s/<id>?token=<token>` off the
/// client's stderr, which is the only place that URL is printed.
fn read_watch_url(stderr: std::process::ChildStderr) -> (String, String, String) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = std::io::BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            use std::io::BufRead;
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    if let Some(parsed) = parse_watch_line(&line) {
                        let _ = tx.send(parsed);
                    }
                }
                Err(_) => break,
            }
        }
    });
    rx.recv_timeout(BOOT_TIMEOUT)
        .expect("client never printed a watch URL")
}

fn parse_watch_line(line: &str) -> Option<(String, String, String)> {
    let start = line.find("http")?;
    let url = line[start..].trim();
    let (base_and_id, token) = url.split_once("?token=")?;
    let (base, id) = base_and_id.split_once("/s/")?;
    Some((base.to_string(), id.to_string(), token.to_string()))
}

/// Strips any `TCOMP_*` the test process inherited (a dev machine's own
/// relay/token config) so the child only sees the flags we pass explicitly.
fn hermetic(cmd: &mut Command) -> &mut Command {
    for (key, _) in std::env::vars() {
        if key.starts_with("TCOMP_") {
            cmd.env_remove(key);
        }
    }
    cmd
}

fn spawn_standalone(bin: &str, name: &str) -> (ClientProcess, String, String) {
    spawn_standalone_mode(bin, name, false)
}

fn spawn_standalone_mode(
    bin: &str,
    name: &str,
    encrypted: bool,
) -> (ClientProcess, String, String) {
    let mut command = Command::new(bin);
    hermetic(&mut command);
    command.env("TCOMP_ENCRYPT", encrypted.to_string());
    command
        .args([
            "standalone",
            "--bind",
            "127.0.0.1",
            "--allow-input",
            "--exit-on-end",
            "--name",
            name,
            "--",
            "bash",
            "--noprofile",
            "--norc",
            "-i",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut guard = Guarded::spawn(&mut command);

    let stdin = guard.0.stdin.take().unwrap();
    let stdout = collect_into(guard.0.stdout.take().unwrap());
    let (base, id, token) = read_watch_url(guard.0.stderr.take().unwrap());
    let (token, key) = match token.split_once("#key=") {
        Some((token, key)) => (token.to_owned(), Some(key.to_owned())),
        None => (token, None),
    };

    (
        ClientProcess {
            guard,
            stdin,
            stdout,
            id,
            key,
        },
        base,
        token,
    )
}

fn spawn_remote(bin: &str, base: &str, token: &str, name: &str) -> ClientProcess {
    spawn_remote_mode(bin, base, token, name, false)
}

fn spawn_remote_mode(
    bin: &str,
    base: &str,
    token: &str,
    name: &str,
    encrypted: bool,
) -> ClientProcess {
    let mut command = Command::new(bin);
    hermetic(&mut command);
    if encrypted {
        command.arg("--encrypt");
    }
    command
        .args([
            "--relay",
            base,
            "--token",
            token,
            "--allow-input",
            "--exit-on-end",
            "--name",
            name,
            "--",
            "bash",
            "--noprofile",
            "--norc",
            "-i",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut guard = Guarded::spawn(&mut command);

    let stdin = guard.0.stdin.take().unwrap();
    let stdout = collect_into(guard.0.stdout.take().unwrap());
    let (_, id, token) = read_watch_url(guard.0.stderr.take().unwrap());
    let key = token.split_once("#key=").map(|(_, key)| key.to_owned());

    ClientProcess {
        guard,
        stdin,
        stdout,
        id,
        key,
    }
}

fn wait_until(mut check: impl FnMut() -> bool, timeout: Duration, what: &str) {
    let deadline = Instant::now() + timeout;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Builds a raw keyboard action sequence, dispatched to whatever element
/// currently has focus. xterm's own input element is off-screen and
/// zero-size (see `.xterm-helper-textarea` in xterm.css), so a coordinate- or
/// element-targeted `send_keys` silently no-ops on it; actions target focus
/// instead of an element, matching how a real keyboard behaves.
fn type_keys(line: &str) -> KeyActions {
    let mut actions = KeyActions::new("keyboard".to_string());
    for key in line.chars() {
        actions = actions.then(KeyAction::Down { value: key });
        actions = actions.then(KeyAction::Up { value: key });
    }
    let enter = Key::Return.into();
    actions
        .then(KeyAction::Down { value: enter })
        .then(KeyAction::Up { value: enter })
}

/// Headless Chrome's synthetic key events don't reliably reach a focused
/// element without a real window manager giving it focus — same reason
/// docs/record-demo.sh renders a normal (non-headless) chromium under Xvfb
/// rather than driving it headless.
fn chromedriver_capabilities() -> Map<String, Value> {
    let mut chrome_options = Map::new();
    chrome_options.insert(
        "args".to_string(),
        Value::Array(vec![
            Value::String("--no-sandbox".to_string()),
            Value::String("--disable-gpu".to_string()),
            Value::String("--window-size=1280,900".to_string()),
            Value::String("--window-position=0,0".to_string()),
        ]),
    );
    let mut capabilities = Map::new();
    capabilities.insert(
        "goog:chromeOptions".to_string(),
        Value::Object(chrome_options),
    );
    capabilities
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs chromium + chromedriver on PATH; run via `nix develop -c cargo test --test floating_windows -- --ignored`"]
async fn floating_windows_type_on_client_and_browser() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let bin = env!("CARGO_BIN_EXE_tcomp");

    let (win1, base, token) = spawn_standalone(bin, "e2e-window-1");
    let win2 = spawn_remote(bin, &base, &token, "e2e-window-2");
    let win3 = spawn_remote(bin, &base, &token, "e2e-window-3");
    let mut windows = [win1, win2, win3];

    let (_chromedriver, client) = browser().await;

    client
        .goto(&format!("{base}/?token={token}"))
        .await
        .expect("open dashboard");

    let deadline = Instant::now() + UI_TIMEOUT;
    loop {
        let shown = client
            .find_all(Locator::Css("#grid.floating .window"))
            .await
            .unwrap_or_default();
        if shown.len() == windows.len() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "dashboard never showed {} floating windows (saw {})",
            windows.len(),
            shown.len()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    for window in &windows {
        client
            .find(Locator::Css(&format!(
                "a.window[data-session-id='{}']",
                window.id
            )))
            .await
            .unwrap_or_else(|_| panic!("no floating window for session {}", window.id));
    }

    let selected = &mut windows[1];
    client
        .find(Locator::Css(&format!(
            "a.window[data-session-id='{}']",
            selected.id
        )))
        .await
        .expect("find the window to select")
        .click()
        .await
        .expect("click the window");

    let deadline = Instant::now() + UI_TIMEOUT;
    loop {
        let url = client.current_url().await.expect("current url");
        if url.path() == format!("/s/{}", selected.id) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "never navigated to the session page"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    let deadline = Instant::now() + UI_TIMEOUT;
    loop {
        let badge = client
            .find(Locator::Css("#badge"))
            .await
            .expect("badge element")
            .text()
            .await
            .expect("badge text");
        if badge.eq_ignore_ascii_case("interactive") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "session page never went live+interactive (badge: {badge})"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    // Type on the client side: a real physical terminal would forward this
    // over the client's own stdin into the pty, same as here.
    selected.type_line("echo tcomp-e2e-from-client");
    let deadline = Instant::now() + UI_TIMEOUT;
    loop {
        let text = client
            .find(Locator::Css("#host"))
            .await
            .expect("terminal host")
            .text()
            .await
            .expect("terminal text");
        if text.contains("tcomp-e2e-from-client") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "client-typed command never showed up in the browser"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Type in the browser: the session page already focused xterm's input
    // element once the session went live and interactive.
    client
        .perform_actions(type_keys("echo tcomp-e2e-from-browser"))
        .await
        .expect("type into the terminal");
    wait_until(
        || selected.stdout_contains("tcomp-e2e-from-browser"),
        UI_TIMEOUT,
        "browser-typed command never showed up on the client",
    );

    client.close().await.expect("close browser session");
}

async fn wait_js(client: &fantoccini::Client, expression: &str) {
    let deadline = Instant::now() + UI_TIMEOUT;
    loop {
        if client
            .execute(&format!("return !!({expression})"), vec![])
            .await
            .ok()
            == Some(Value::Bool(true))
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "browser condition timed out: {expression}; page: {:?}",
            client
                .execute("return document.body.innerText", vec![])
                .await
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs chromium + chromedriver + xvfb-run"]
async fn encrypted_sessions_unlock_roundtrip_and_replay() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let bin = env!("CARGO_BIN_EXE_tcomp");
    let (mut host, base, token) = spawn_standalone_mode(bin, "private-encrypted-name", true);
    let key = host.key.clone().expect("key in watch fragment");
    let plain = spawn_remote(bin, &base, &token, "ordinary-session");
    host.type_line("printf 'secret-terminal-output\\n'");
    let (_driver, client) = browser().await;
    let session_url = format!("{base}/s/{}", host.id);
    client
        .goto(&format!("{session_url}?token={token}"))
        .await
        .unwrap();
    wait_js(
        &client,
        "document.getElementById('notice').textContent.includes('complete watch link')",
    )
    .await;
    assert_eq!(
        client.execute("return allowInput", vec![]).await.unwrap(),
        false
    );
    client
        .goto(&format!("{session_url}#key={}", "A".repeat(43)))
        .await
        .unwrap();
    client.refresh().await.unwrap();
    wait_js(
        &client,
        "document.getElementById('notice').textContent.includes('wrong key')",
    )
    .await;
    client
        .goto(&format!("{session_url}#key={key}"))
        .await
        .unwrap();
    client.refresh().await.unwrap();
    wait_js(
        &client,
        "document.getElementById('name').textContent === 'private-encrypted-name' && allowInput",
    )
    .await;
    wait_js(
        &client,
        "document.querySelector('.xterm-rows').textContent.includes('secret-terminal-output')",
    )
    .await;
    client
        .execute(
            "send(\"printf 'browser-secret-%s\\\\n' 'roundtrip'\\r\")",
            vec![],
        )
        .await
        .unwrap();
    wait_until(
        || host.stdout_contains("browser-secret-roundtrip"),
        UI_TIMEOUT,
        "encrypted browser input",
    );
    client.execute("socket.close()", vec![]).await.unwrap();
    wait_js(
        &client,
        "socket.readyState === WebSocket.OPEN && allowInput",
    )
    .await;
    wait_js(
        &client,
        "document.querySelector('.xterm-rows').textContent.includes('browser-secret-roundtrip')",
    )
    .await;
    let sessions = client
        .execute_async(
            "const done = arguments[0]; fetch('/api/sessions').then(r => r.json()).then(done)",
            vec![],
        )
        .await
        .unwrap();
    let sessions = sessions.as_array().unwrap();
    let encrypted = sessions.iter().find(|s| s["id"] == host.id).unwrap();
    assert_eq!(encrypted["encryption"], 2);
    assert_eq!(encrypted["preview"], "");
    assert_eq!(encrypted["cmd"], "");
    assert!(!encrypted.to_string().contains("private-encrypted-name"));
    assert!(!encrypted.to_string().contains("secret-terminal-output"));
    assert!(!encrypted.to_string().contains(&key));
    assert!(sessions
        .iter()
        .any(|s| s["id"] == plain.id && s["name"] == "ordinary-session"));
    client.goto(&base).await.unwrap();
    wait_js(
        &client,
        "document.body.innerText.includes('End-to-end encrypted')",
    )
    .await;
    client.goto(&session_url).await.unwrap();
    wait_js(
        &client,
        "document.getElementById('name').textContent === 'private-encrypted-name' && allowInput",
    )
    .await;
    client.delete_all_cookies().await.unwrap();
    client
        .execute("sessionStorage.clear()", vec![])
        .await
        .unwrap();
    client
        .goto(&format!("{session_url}#key={key}"))
        .await
        .unwrap();
    client.refresh().await.unwrap();
    client
        .find(Locator::Css("#token"))
        .await
        .unwrap()
        .send_keys("wrong-token")
        .await
        .unwrap();
    client
        .find(Locator::Css("button[type=submit]"))
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    wait_js(
        &client,
        "new URLSearchParams(location.search).has('bad') && document.readyState === 'complete'",
    )
    .await;
    client
        .find(Locator::Css("#token"))
        .await
        .unwrap()
        .send_keys(&token)
        .await
        .unwrap();
    client
        .find(Locator::Css("button[type=submit]"))
        .await
        .unwrap()
        .click()
        .await
        .unwrap();
    wait_js(
        &client,
        "document.getElementById('name').textContent === 'private-encrypted-name' && allowInput",
    )
    .await;
    assert!(!client
        .current_url()
        .await
        .unwrap()
        .query()
        .unwrap_or_default()
        .contains(&key));
    let vector = client.execute_async(r#"
        const [encoded, done] = arguments;
        (async () => {
          const raw = Uint8Array.from(atob(encoded.replaceAll('-', '+').replaceAll('_', '/')), c => c.charCodeAt(0));
          const key = TcompCrypto.encode(new Uint8Array(32).fill(7));
          const cipher = await TcompCrypto.create(key, 'vector-session');
          const payload = await cipher.open(raw);
          if (payload.screen !== 'hello €') throw new Error('vector mismatch');
          if (await cipher.open(raw) !== null) throw new Error('replay accepted');
          for (const kind of ['tamper', 'version', 'short', 'session', 'key']) {
            const candidate = kind === 'short' ? raw.slice(0, 10) : raw.slice();
            if (kind === 'tamper') candidate[candidate.length - 1] ^= 1;
            if (kind === 'version') candidate[4] = 2;
            const reader = await TcompCrypto.create(kind === 'key' ? TcompCrypto.encode(new Uint8Array(32).fill(8)) : key, kind === 'session' ? 'other' : 'vector-session');
            let rejected = false;
            try { await reader.open(candidate); } catch (_) { rejected = true; }
            if (!rejected) throw new Error(kind + ' accepted');
          }
          return true;
        })().then(done, error => done(error.message));
    "#, vec![Value::String(include_str!("../src/crypto-vector.txt").trim().into())]).await.unwrap();
    assert_eq!(vector, true);
    let reinitialize = client.execute_async(r#"
        const done = arguments[0];
        const original = cipher;
        receive(socket, { data: JSON.stringify({ t: 'init', encryption: 2 }) }).then(
          () => done('accepted repeated initialization'),
          error => done(error.message === 'Unexpected session reinitialization.' && cipher === original),
        );
    "#, vec![]).await.unwrap();
    assert_eq!(reinitialize, true);
    let mut remote = spawn_remote_mode(bin, &base, &token, "private-remote-name", true);
    let remote_url = format!(
        "{base}/s/{}#key={}",
        remote.id,
        remote.key.as_ref().unwrap()
    );
    remote.type_line("printf '\\033[?1049h\\033[2J\\033[Halternate-screen-secret'");
    client.goto(&remote_url).await.unwrap();
    wait_js(&client, "document.querySelector('.xterm-rows').textContent.includes('alternate-screen-secret') && allowInput").await;
    remote.type_line("printf '\\033[?1049l'; printf '\\033]0;private-title\\007'");
    wait_js(
        &client,
        "document.getElementById('name').textContent === 'private-title'",
    )
    .await;
    client.refresh().await.unwrap();
    wait_js(
        &client,
        "document.getElementById('name').textContent === 'private-title' && allowInput",
    )
    .await;
    remote.type_line("printf 'final-encrypted-output\\n'; exit 7");
    wait_js(
        &client,
        "status === 'ended' && document.getElementById('notice').textContent.includes('exit 7')",
    )
    .await;
    client.refresh().await.unwrap();
    wait_js(&client, "status === 'ended' && document.querySelector('.xterm-rows').textContent.includes('final-encrypted-output')").await;
    client.close().await.unwrap();
}

async fn browser() -> (Guarded, fantoccini::Client) {
    let chromedriver_port = free_port();
    let mut chromedriver = Command::new("xvfb-run");
    chromedriver
        .args([
            "-a",
            "-s",
            "-screen 0 1280x900x24",
            "chromedriver",
            &format!("--port={chromedriver_port}"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _chromedriver = Guarded::spawn(&mut chromedriver);

    let client = {
        let deadline = Instant::now() + BOOT_TIMEOUT;
        loop {
            match ClientBuilder::rustls()
                .expect("rustls tls backend")
                .capabilities(chromedriver_capabilities())
                .connect(&format!("http://127.0.0.1:{chromedriver_port}"))
                .await
            {
                Ok(client) => break client,
                Err(error) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    let _ = error;
                }
                Err(error) => panic!("could not connect to chromedriver: {error}"),
            }
        }
    };

    (_chromedriver, client)
}
