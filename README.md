# preview

Runs any web app on a free port, restarts it cleanly, and tells you where it listens. Made for
coding agents that share one computer across many chats, and for people at a terminal.

```sh
cd ~/apps/shop && preview run -- bun --env-file=.env run dev
# {"name":"apps-shop","port":5173,"ports":[5173],"pid":412,"url":"http://localhost:5173","log":"/home/user/.runable/previews/apps-shop.log"}
```

Run the same command again to restart: `preview` stops the whole earlier process tree, waits for
its port, and starts fresh, usually on the same port. When something goes wrong, it prints one
message that ends with what to do next.

It is a single native binary for Linux (x64, arm64, static) and macOS (x64, arm64). It needs no
Node or Bun at run time.

## Install

```sh
npm i -g @runablehq/preview        # or: bun add -g @runablehq/preview
curl -fsSL https://github.com/runablehq/preview/releases/latest/download/install.sh | sh
```

The installer puts the binary in `~/.local/bin` and checks its SHA-256. It reads
`PREVIEW_INSTALL_DIR`, `PREVIEW_VERSION` and `PREVIEW_DOWNLOAD_URL` (for a mirror).

## Commands

```text
preview run [--name <name>] [--port <port>] [--wait <seconds>] [--] <command>...
preview stop <name>
preview stop --port <port>
preview list
preview logs <name> [--lines <count>]
preview upgrade
preview --version
```

### `preview run`

1. Names the run after the current folder relative to `$HOME` (`~/apps/shop` is `apps-shop`),
   or `--name`.
2. Stops the earlier run of that name: SIGTERM to its process group, up to 2 s, then SIGKILL.
3. Picks a port: `--port`, else this name's last port, else a free port in 3000–9999 that no
   other preview remembers.
4. Starts the command in its own session, with its output in the log and these variables set:

   | Variable | Value |
   | --- | --- |
   | `PORT` | the chosen port |
   | `HOST` | `PREVIEW_HOST`, default `0.0.0.0` on Linux and `127.0.0.1` on macOS |
   | `BROWSER` | `none` |
   | `PYTHONUNBUFFERED` | `1` |
   | `NG_CLI_ANALYTICS` | `false` |

5. Waits (up to `--wait`, default 45 s) until anything in its process tree listens on a TCP
   port. Then it prints the JSON line and exits.

A single argument runs as a bash script, so `source .venv/bin/activate && flask run` works.
With several arguments, `$PORT`, `${PORT}`, `$HOST` and `${HOST}` inside them are filled in.
Quote them so your own shell leaves them alone:

```sh
preview run 'source .venv/bin/activate && flask run --port $PORT --host 0.0.0.0'
preview run -- python3 -m http.server '$PORT'
```

The JSON line:

| Field | Meaning |
| --- | --- |
| `name` | the preview's name, for `stop` and `logs` |
| `port` | the port to open: the offered port if the app listens on it, else its lowest port |
| `ports` | every TCP port the tree listens on |
| `pid` | the process group `preview stop` signals |
| `url` | `http://localhost:<port>` |
| `log` | the app's output |

Fields are only ever added, never renamed or removed.

Exit status: `0` listening; `1` failed (the message says why and what to do); `2` still
starting when `--wait` ran out. On `2` the app keeps running, so check it with `preview list`.

### What the failures look like

```text
$ preview run python3 -m http.server 8765
`python3 -m http.server 8765` exited (code 1) before listening on a port. Last lines of …/blog.log:
  OSError: [Errno 98] Address already in use
Port 8765 is held by preview shop (pid 4120: python3 -m http.server 8765). Stop it with preview stop shop, or free the port with preview stop --port 8765.

$ preview run 'source .venv/bin/activate && flask run'
Port 5000 listens only on 127.0.0.1, which a preview cannot reach. Start the app bound to 0.0.0.0, for example flask run --port $PORT --host 0.0.0.0, then run the same preview run again.
```

The bind hint comes from the command, including the `package.json` script that `npm run`,
`pnpm`, `yarn` or `bun` runs. It covers Flask, Django, uvicorn, Streamlit, Gradio, Vite, Astro,
Angular, webpack and Rails.

### `preview stop`, `list`, `logs`

- `preview stop <name>` stops that preview and keeps its remembered port.
- `preview stop --port <port>` stops whatever listens there. If a preview holds the port, it
  stops that preview. Otherwise it stops the server's own process group, or only the server and
  its children when that group also holds other work (an agent's shell, say).
- `preview list` prints a JSON array with one object per name ever run: `name`, `running`,
  `ports` (live), `port` (remembered), `pid` (0 when stopped), `cwd`, `command` and `log`.
- `preview logs <name>` prints the last 40 lines of the log (`--lines` changes that).

## For agents

1. Start every server with `preview run` from the app's folder. Run the same command again to
   restart it.
2. Expose the `port` it printed.
3. When the framework ignores `PORT`, put `--port $PORT --host 0.0.0.0` (or its equivalent) in
   the command, inside single quotes.
4. On an error, do what its last line says.

## State

State lives in `PREVIEW_HOME`, default `~/.runable/previews`. Each name has these files:

- `<name>.state`: `key=value` lines, written atomically.
- `<name>.log`: emptied on each run.
- `<name>.lock`: `run` and `stop` hold it while they work, so two calls for one name take turns.

Every version reads the state files of every older version.

## Development

```sh
cargo test
cargo build --release && PREVIEW=target/release/preview tests/smoke.sh   # SMOKE_VITE=1 adds the Vite case
```

`tests/smoke.sh` runs the scenarios from the design against a real binary. CI runs it on Linux
and macOS for every PR; the release runs it again against the package installed from npm.
Releasing is described in [RELEASING.md](RELEASING.md).

## Updates

`preview` keeps itself on npm's `latest` with no setup, however it was installed (npm, curl,
a binary copied into an image, a read-only `/usr/local/bin`):

- After any command, when the last check is more than 6 hours old, it starts a detached
  background check and exits without waiting. The check downloads the new version of this
  platform's package, checks its sha512 against the registry, checks that it runs, and
  installs it under `~/.runable/preview/versions/`.
- At start, a binary hands off to the newest installed version, so the next command runs it.
- A rollback works the same way: deprecate the bad version on npm and move `latest` back.
  Machines on the deprecated version switch to `latest` at their next check.

Updates never write to stdout, never slow a command down, and never touch running previews.

| Control | Effect |
| --- | --- |
| `PREVIEW_UPDATE=0` | no checks, no hand-off: the installed binary runs as is |
| `PREVIEW_VERSION=<v>` | run exactly `<v>`, installing it if needed |
| `PREVIEW_REGISTRY=<url>` | fetch from a registry mirror instead of registry.npmjs.org |
| `preview upgrade` | check now, in the foreground, and say what happened |
| `preview --version` | `preview 0.4.1 (installed 0.3.0, checked 2 h ago)` |

Downloads honor `HTTPS_PROXY`, `ALL_PROXY` and `NO_PROXY`. They trust the system's
certificates first, then Mozilla's bundled roots.

## Not yet

Updates are not signed yet. A version is trusted if npm serves it with a matching sha512, and
releases come only from this repository's release workflow through npm trusted publishing.
Also missing: adoption of servers that detach from the tree (pm2, `docker compose up -d`), the
host-check probe (`hostCheck`), and HTTP-based port choice. Windows is not supported.
