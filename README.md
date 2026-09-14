# tcomp — terminal companion

Run a command locally; watch it read-only in a browser.

![tcomp mirroring a live terminal into the browser](docs/demo.gif)

```sh
tcomp standalone -- claude                          # self-contained, prints a URL
tcomp --relay https://tcomp.example.com -- claude   # via a shared relay
```

The command runs exactly as it would in your terminal. The browser view is
read-only by default, at the host's exact dimensions, and works on a phone.

## Security

**There is no authentication.** Anyone who can reach the relay sees everything
the command prints — file contents, tokens, keys. `--allow-input` raises that to
**arbitrary code execution**: they can type into your terminal as you.

Run the relay somewhere private or behind an authenticating proxy.
`crates/tcomp/src/server/auth.rs` is the single seam for adding auth.

## Modes

| Mode | Command | Notes |
| --- | --- | --- |
| standalone | `tcomp standalone -- <cmd>` | embedded relay, loopback unless you `--bind` |
| client | `tcomp --relay URL -- <cmd>` | dials *out*, so NAT and VPNs are fine |
| server | `tcomp serve` | be the relay; this is what the Docker image runs |

## Client flags

| Flag | Env | Meaning |
| --- | --- | --- |
| `--relay` | `TCOMP_RELAY` | relay base URL; required unless `standalone` |
| `--name` | `TCOMP_NAME` | label in the web UI (default: hostname) |
| `--allow-input` | `TCOMP_ALLOW_INPUT` | let the browser type into this terminal |
| `--bind` | `TCOMP_BIND` | `standalone`: listen address (default `127.0.0.1:0`); a bare IP takes a random port |
| `--public-url` | `TCOMP_PUBLIC_URL` | `standalone`: base URL for the watch link (default: the bound address) |

The child's exit code is the wrapper's. Dropped connections reconnect and resume;
status lines go to stderr, never into the stream the browser sees.

## Relay

`tcomp serve` is configured by environment: `TCOMP_BIND` (`0.0.0.0:8080`),
`TCOMP_PUBLIC_URL` (default: the bound address), `TCOMP_WEB_DIR` (`web`),
`TCOMP_ENDED_TTL` (`60`), `TCOMP_STALE_TTL` (`14400`), `TCOMP_SCROLLBACK`
(`2000`), `TCOMP_HISTORY_BYTES` (`524288`). Terminate TLS in a proxy in front.

A session is live while the producer is connected, `finished` once the command
exits, and `disconnected` if the producer vanishes — greyed out, history only,
and live again by itself if the client reconnects.

## Tailscale

`--bind` the tailnet address and the URL tcomp prints works from any device on
the tailnet:

```sh
tcomp standalone --bind "$(tailscale ip -4)" -- claude
```

For TLS and a stable `<machine>.<tailnet>.ts.net` name, leave the relay on
loopback and `tailscale serve --bg <port>` in front of it instead. Everyone on
the tailnet reaches every session — see Security. Never `tailscale funnel` it.

## Development

Rust is not on `PATH` outside the Nix shell.

```sh
nix develop
cargo test
cargo clippy --all-targets -- -D warnings
cargo run -p tcomp -- standalone -- htop
```

`web/` is read from disk per request, so viewer changes need no rebuild.
`docs/record-demo.sh` regenerates the GIF above.
