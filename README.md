# tcomp — take your terminals on the go

Start a session on your laptop and carry on with it from anywhere. Read-only by
default, two-way with `--allow-input`.

![tcomp mirroring a live terminal into the browser](docs/demo.gif)

## Features

**standalone** — embedded relay on a loopback port, nothing to deploy. Prints the
watch URL and runs your command.

```sh
tcomp standalone -- claude
```

**client** — use a relay someone else runs. The wrapper dials *out*, so it opens
no inbound port and works behind NAT, on a customer network, or on a VPN.

```sh
tcomp --relay https://tcomp.example.com -- claude
```

**server** — be that relay, serving the viewer and fanning out to browsers. This
is what the Docker image runs.

```sh
TCOMP_TOKEN=$(openssl rand -hex 16) tcomp serve
```

**interactive** — let the browser type into the terminal instead of just
watching. Off by default, decided by the host, and never safe on a relay you
would not hand a shell to.

```sh
tcomp standalone --allow-input -- claude
```

## Installing

**nix** — the flake exposes the binary as `packages.default` and carries `web/`
along with it, so the viewer is served wherever you run it from.

```sh
nix run github:flodurorg/tcomp -- standalone -- claude
```

On NixOS, take the flake as an input and put the package on the system:

```nix
inputs.tcomp.url = "github:flodurorg/tcomp";
inputs.tcomp.inputs.nixpkgs.follows = "nixpkgs";

environment.systemPackages = [
  inputs.tcomp.packages.${pkgs.stdenv.hostPlatform.system}.default
];
```

## Security

**A relay is only as private as its token.** Anyone holding it sees everything
the command prints — file contents, tokens, keys — and on an `--allow-input`
session can type into your terminal as you.

`tcomp standalone` mints a fresh token per run and carries it in the printed
watch URL, so an embedded relay is never open — not even when you `--bind` it to
a tailnet address. `tcomp serve` is **open until you set `TCOMP_TOKEN`**; set
one, or keep the relay somewhere private or behind an authenticating proxy. It
says so in the log on every start.

Opening a `?token=…` link moves the token into an `HttpOnly`, `SameSite=Lax`
cookie and redirects to the clean URL, so the secret leaves the address bar and
never rides along in a `Referer`. Without a link you get a sign-in page instead.
Scripts can send `Authorization: Bearer <token>`. Tokens must be URL-safe —
letters, digits and `-._~` — and the relay refuses to start otherwise.
`/healthz` and `/static` stay open.

`src/server/auth.rs` is still the single seam, and what it
implements is one shared token: access is all-or-nothing, with no per-session
scope and no separate read-only credential.

## Deploying

**tailnet** — `--bind` the tailnet address and the printed URL works from any
device on the tailnet. For TLS and a stable `<machine>.<tailnet>.ts.net` name,
leave the relay on loopback and put `tailscale serve --bg <port>` in front of it
instead. Never `tailscale funnel` it.

```sh
tcomp standalone --bind "$(tailscale ip -4)" -- claude
```

**docker compose** — the image runs `tcomp serve`. Compose publishes `8080` and
mounts `web/` read-only; set `TCOMP_PUBLIC_URL` to the address people will
actually open.

```sh
TCOMP_PUBLIC_URL=https://tcomp.example.com docker compose up -d
```

**kubernetes** — the chart in `charts/tcomp-chart` is published to
`oci://ghcr.io/flodurorg/tcomp/tcomp-chart` and runs one replica, because
sessions live in memory and are not shared between pods. `image.tag` follows the
chart's `appVersion`, so a release ships a chart that pins its own image.

```sh
helm install tcomp oci://ghcr.io/flodurorg/tcomp/tcomp-chart \
  --set ingress.enabled=true \
  --set ingress.hosts[0].host=tcomp.example.com
```

A token is generated if you do not set one, and reused on upgrade. Read it with
`kubectl get secret tcomp -o jsonpath='{.data.token}' | base64 -d`, or set
`auth.token` / `auth.existingSecret` yourself.

Gateway API works instead of an Ingress, and the two are independent:

```sh
helm install tcomp oci://ghcr.io/flodurorg/tcomp/tcomp-chart \
  --set httpRoute.enabled=true \
  --set httpRoute.parentRefs[0].name=my-gateway \
  --set httpRoute.hostnames[0]=tcomp.example.com
```

`publicUrl` is derived from the first ingress host or `httpRoute` hostname when
left empty; set it explicitly when a proxy sits in front. Some Gateway
implementations apply a default request timeout that would cut the websocket;
set `httpRoute.timeouts.request=0s` if viewers drop.

## Parameters

Session flags, each with an environment equivalent:

| Flag | Env | Meaning |
| --- | --- | --- |
| `--relay` | `TCOMP_RELAY` | relay base URL; required unless `standalone` |
| `--name` | `TCOMP_NAME` | label in the web UI (default: hostname) |
| `--allow-input` | `TCOMP_ALLOW_INPUT` | let the browser type into this terminal |
| `--token` | `TCOMP_TOKEN` | token to present to the relay |

Relay parameters, which configure whichever relay is running — the one embedded
in `standalone` or a separate `tcomp serve`. `standalone` takes the first two as
flags as well:

| Flag | Env | Default | Meaning |
| --- | --- | --- | --- |
| `--bind` | `TCOMP_BIND` | `127.0.0.1:0` embedded, `0.0.0.0:8080` serving | listen address; a bare IP takes a random port |
| `--public-url` | `TCOMP_PUBLIC_URL` | the bound address | base URL session links are built from |
| | `TCOMP_TOKEN` | none for `serve`, minted for `standalone` | token the relay requires; unset leaves `serve` open |
| | `TCOMP_WEB_DIR` | `web` | directory holding the viewer pages |
| | `TCOMP_ENDED_TTL` | `60` | seconds a cleanly-exited session is kept |
| | `TCOMP_STALE_TTL` | `14400` | seconds a disconnected session is kept |
| | `TCOMP_SCROLLBACK` | `2000` | scrollback lines kept server-side |
| | `TCOMP_HISTORY_BYTES` | `524288` | replay buffer per session |

## Sessions

| In the UI | Status | When | Kept for |
| --- | --- | --- | --- |
| live | `live` | producer connected | while connected |
| interactive | `live` | producer connected, started with `--allow-input` | while connected |
| finished | `ended` | the command exited and the relay was told | `TCOMP_ENDED_TTL` |
| disconnected | `stale` | the producer vanished without saying goodbye | `TCOMP_STALE_TTL` |

A disconnected session is greyed out and history only, and goes live again by
itself if the client reconnects. Input is refused whenever a session is not live.
The child's exit code is the wrapper's, and status lines go to stderr, never into
the stream the browser sees.

## Development

```sh
nix develop
cargo test
cargo clippy --all-targets -- -D warnings
cargo run -p tcomp -- standalone -- htop
```

`web/` is read from disk per request, so viewer changes need no rebuild.
`docs/record-demo.sh` regenerates the GIF above.
