# tcomp — terminal companion

Run a command locally; watch it read-only in a browser.

```
tcomp standalone -- claude                          # self-contained, prints a URL
tcomp --relay https://tcomp.example.com -- claude   # via a shared relay
```

The command runs exactly as it would in your terminal. At the same time a
read-only live view is available at a URL served by the relay.

One binary, three modes:

| Mode | Command | Use |
| --- | --- | --- |
| standalone | `tcomp standalone -- <cmd>` | embedded relay on a random loopback port; nothing to deploy |
| client | `tcomp --relay URL -- <cmd>` | connect to a relay someone else is running |
| server | `tcomp serve` | be that relay (this is what the Docker image runs) |

**No inbound port is opened on the workstation.** The wrapper dials *out* to the
relay, so it works behind NAT, on a customer network, or on someone else's VPN.

## Security

There is **no authentication**. The home page lists every session, so anyone who
can reach the relay sees everything your terminal prints — file contents, tokens,
keys, whatever the command echoes. Session IDs are 128-bit random, but with an
index page that is not a control.

`--allow-input` raises that from disclosure to **arbitrary code execution**:
anyone who can reach the relay can type into that terminal as you. It is off by
default and decided by the host, never by the relay or the browser. Do not use it
on a relay that is reachable by anyone you would not hand a shell.

Run the relay somewhere private, or put an authenticating proxy in front of it,
until auth lands. `crates/tcomp/src/server/auth.rs` is the single seam: it is
called on the index, on `/s/<id>`, on `/ws/produce`, and separately for write
access, so read-only and read-write can be gated independently.

`tcomp standalone` binds loopback only, so it is reachable from your machine
until you deliberately expose it (a tunnel, an SSH forward, a wider bind).

## Layout

| Path | What |
| --- | --- |
| `crates/tcomp` | the binary — pty wrapper, and `server/` holding the relay |
| `crates/tcomp/src/server` | axum routes, session state, viewer fan-out |
| `crates/proto` | wire types shared by client and server |
| `web/` | viewer pages + vendored xterm.js, no build step |

## Development

Rust is not on `PATH` outside the Nix shell.

```sh
nix develop
cargo test
cargo clippy --all-targets -- -D warnings
```

Everything in one process:

```sh
cargo run -p tcomp -- standalone -- htop
```

Or a relay plus a separate session, which is what production looks like:

```sh
cargo run -p tcomp -- serve                                # :8080
cargo run -p tcomp -- --relay http://localhost:8080 -- htop
```

Or run the relay in Docker:

```sh
docker compose up --build          # :8080
docker compose watch               # same, reloads web/ on edit
```

`web/` is bind-mounted and read from disk per request, so viewer changes need no
rebuild — reload the page. Rust changes need `--build`.

## Client

```
tcomp --relay URL [--name NAME] [--token TOKEN] [--allow-input] -- <command>...
tcomp standalone   [--name NAME] [--token TOKEN] [--allow-input] -- <command>...
```

| Flag | Env | Meaning |
| --- | --- | --- |
| `--relay` | `TCOMP_RELAY` | relay base URL; required unless you use `standalone` |
| `--name` | `TCOMP_NAME` | label in the web UI (default: hostname) |
| `--token` | `TCOMP_TOKEN` | sent in the hello frame; ignored in v1 |
| `--allow-input` | `TCOMP_ALLOW_INPUT` | let the web view type into this terminal (see Security) |

The child's exit code is the wrapper's exit code. If the connection drops the
client reconnects with exponential backoff and resumes the same session. If the
relay has forgotten that session — it restarted, or the TTL ran out — the client
says so and prints the new URL rather than reattaching silently.

Status lines go to stderr, never into the stream the browser sees, so they cannot
be confused with the command's own output. They are coloured when stderr is a
terminal and plain otherwise, honouring `NO_COLOR` and `TERM=dumb`.

## Relay

`tcomp serve` reads its configuration from the environment:

| Env | Default | Meaning |
| --- | --- | --- |
| `TCOMP_BIND` | `0.0.0.0:8080` | listen address |
| `TCOMP_PUBLIC_URL` | `http://localhost:8080` | used to build session URLs |
| `TCOMP_WEB_DIR` | `web` | directory holding the viewer pages |
| `TCOMP_ENDED_TTL` | `60` | seconds a cleanly-exited session is kept |
| `TCOMP_STALE_TTL` | `14400` | seconds a disconnected session is kept |
| `TCOMP_SCROLLBACK` | `2000` | scrollback lines kept server-side |
| `TCOMP_HISTORY_BYTES` | `524288` | replay buffer per session |

TLS is expected to terminate in a reverse proxy in front of the relay.

## Session states

| State | When | In the UI |
| --- | --- | --- |
| live | producer connected | coloured, streaming |
| interactive | producer connected, started with `--allow-input` | as live, plus the browser can type |
| finished | command exited, relay told | kept `TCOMP_ENDED_TTL`, then dropped |
| disconnected | producer vanished without saying goodbye | greyed out, still clickable, history only, kept `TCOMP_STALE_TTL` |

A disconnected session goes live again by itself if the client reconnects.

## Viewing

`/` lists sessions as terminal-window cards with a live preview. Opening one
shows that terminal alone at the host's exact dimensions, scaled to fit the
viewport — the grid is never reflowed, so what you see matches the host
character for character. Works on a phone.

A banner above the terminal names the session and says whether it takes input.
It rides its own control frame rather than the byte stream, so tcomp's own
messages never end up in the terminal's scrollback, and it is replayed on join
so it is there however late you open the page.

Without `--allow-input` the browser never sends input, and the viewer socket
discards anything it receives.

With `--allow-input` the terminal takes keystrokes and forwards them to the pty.
A key bar supplies esc, tab, ^C, ^D, ^Z and arrows, which phone keyboards lack.
Every viewer of that session can type — there is no writer lock, so two people
typing at once interleave their bytes. Input is refused once a session stops
being live, so a disconnected session cannot be typed into while it waits out its
TTL. The browser still cannot resize the host.
