# single-ion — User Guide

`single-ion` is a single self-contained binary that runs all four Free Radicals services
(**reactive**, **ion**, **gluon**, **neutrino**) as concurrent tasks inside one process.
It is intended for single-node deployments, developer workstations, and edge installations
where running four separate processes is undesirable.

The distributed deployment model is fully preserved — each service still binds its own port,
uses its own config, and can be moved back to a separate process at any time without any code
changes.

---

## Contents

1. [Prerequisites](#prerequisites)
2. [Building](#building)
3. [Windows desktop app (`single-ion-win`)](#windows-desktop-app-single-ion-win)
4. [Running](#running)
5. [Ports](#ports)
6. [Configuration](#configuration)
   - [Working directory](#working-directory)
   - [Reactive (`cfg/config.yaml`)](#reactive-cfgconfigyaml)
   - [Ion (`cfg/ion.yaml`)](#ion-cfgionyaml)
   - [Gluon (`cfg/gluon.yaml`)](#gluon-cfggluonyaml)
   - [Neutrino (`cfg/neutrino.yaml`)](#neutrino-cfgneutrinoyaml)
7. [Environment variables](#environment-variables)
8. [Logging](#logging)
9. [Shutdown](#shutdown)
10. [Moving to distributed deployment](#moving-to-distributed-deployment)

---

## Prerequisites

| Requirement | Notes |
|---|---|
| Rust toolchain (stable) | Install via [rustup.rs](https://rustup.rs) |
| Ion static assets | `ion/static/` must be accessible at the path configured in `cfg/ion.yaml` |
| Neutrino templates | `neutrino/templates/` must be accessible at the path configured in `cfg/neutrino.yaml` |
| Kubernetes access (optional) | Required only if neutrino is expected to manage deployments |

---

## Building

```bash
cd single-ion
cargo build --release
```

The compiled binary is written to `../target/release/single-ion` (one level above the
`single-ion/` package directory, in the standard Cargo target directory).

For development:

```bash
cd single-ion
cargo build          # debug build, faster to compile
```

---

## Windows desktop app (`single-ion-win`)

`single-ion-win` is a Windows-only variant that opens a native **WebView2** window instead of
requiring a separate browser.  All four services run identically to the headless binary, but
every service is bound exclusively to **loopback (`127.0.0.1`)** using OS-assigned ephemeral
ports — no fixed port numbers, no port conflicts, nothing reachable from the network.

### Additional prerequisites

| Requirement | Notes |
|---|---|
| WebView2 Runtime | Ships with Windows 10 1803+ and any machine that has Edge installed. Download the bootstrapper from [Microsoft](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) if needed. |

### Building

```bash
cd single-ion
cargo build --release --bin single-ion-win --features windows-app
```

The binary is written to `../target/release/single-ion-win.exe`.

For development (keeps the console window visible so tracing output is readable):

```bash
cd single-ion
cargo build --bin single-ion-win --features windows-app
```

### Running

Run from the `single-ion/` directory, exactly like the headless binary:

```bash
cd single-ion
../target/release/single-ion-win.exe
```

On startup the binary:

1. Acquires a named Windows mutex (`Global\single-ion`) — a second launch exits immediately
   rather than starting a conflicting instance.
2. Reserves five ephemeral loopback ports from the OS (one each for Gluon, Reactive HTTP,
   Reactive pgwire, ION, and Neutrino).
3. Injects the resolved port numbers as environment variables so every service's config
   system picks them up automatically — no changes to `cfg/*.yaml` are needed.
4. Starts all four services in a background thread.
5. Waits up to 30 seconds for ION to accept connections, then opens the WebView2 window
   pointing at `http://127.0.0.1:<ion-port>`.

Closing the window shuts the process down cleanly.

### Port behaviour

Because ports are OS-assigned at runtime, they are not fixed or predictable.  All
inter-service URLs are wired automatically via environment variables — you do not need to
set them manually.  The pgwire endpoint is still available on its assigned loopback port if
you need SQL access; check the startup log for the actual port number:

```
INFO single-ion-win: ports gluon=49821 reactive=49822 pgwire=49823 ion=49824 neutrino=49825
```

### Logging

In release builds the console window is hidden (`windows_subsystem = "windows"`), so
`FR_LOG` output is silently discarded.  Debug builds keep the console.  To capture logs
from a release build, redirect stderr to a file by launching from a terminal:

```powershell
../target/release/single-ion-win.exe 2> single-ion.log
```

---

## Running

Run from the `single-ion/` directory so that the `cfg/` config files and relative asset
paths are resolved correctly:

```bash
cd single-ion
cargo run --bin single-ion          # development (debug build)

# — or, after a release build —
../target/release/single-ion
```

On startup you will see log lines from each service as it binds its port:

```
INFO single-ion: loading service configs
INFO single-ion: spawning services
INFO gluon: gluon listening on 0.0.0.0:4747
INFO reactive: reactive starting http=4749 pgwire=Some(5433) db_root="dbs"
INFO ion: ion listening on http://0.0.0.0:8080
INFO neutrino: neutrino listening on 0.0.0.0:4748
```

Once all four lines appear the stack is fully up.  Open your browser at
**http://localhost:8080** to reach the Ion workbench.

**First run — default admin credentials**

On first startup Reactive automatically provisions a default admin account using the
credentials set in `cfg/config.yaml` (fields `security.admin_user` and
`security.admin_password`).  The shipped defaults are:

| Field | Default |
|---|---|
| Username | `admin` |
| Password | `admin` |

Log in at **http://localhost:8080** with these credentials.  Change the password after
first login, or set different credentials before the first run via environment variables:

```bash
REACTIVE__SECURITY__ADMIN_USER=myuser \
REACTIVE__SECURITY__ADMIN_PASSWORD=mysecret \
cargo run --bin single-ion
```

> **Security note:** The shipped `admin`/`admin` defaults are for development only.
> Always change them before exposing the service on a network.

To check service connectivity and configuration, visit **http://localhost:8080/status**
(no login required).

---

## Ports

| Service | Port | Protocol | Purpose |
|---|---|---|---|
| Ion | **8080** | HTTP / WebSocket | Browser UI entry point |
| Reactive | **4749** | HTTP / WebSocket | Database API (internal — not exposed to the browser) |
| Reactive | **5433** | pgwire | PostgreSQL-compatible SQL endpoint |
| Gluon | **4747** | WebSocket / HTTP | Pub/sub event bus |
| Neutrino | **4748** | HTTP | Kubernetes deployment manager |

All ports are configurable in the respective `cfg/*.yaml` files described below.

---

## Configuration

### Working directory

Each service resolves its config file and asset paths **relative to the process working
directory**.  Always run the binary from the `single-ion/` directory (or set the working
directory to it in your service manager / Docker container).

```
single-ion/
  cfg/
    config.yaml      ← reactive
    ion.yaml         ← ion
    gluon.yaml       ← gluon
    neutrino.yaml    ← neutrino
```

These files are present in the repository with sensible defaults.  Edit them as needed —
changes take effect on the next restart.

---

### Reactive (`cfg/config.yaml`)

All fields in `cfg/config.yaml` must be present — reactive's config structs do not apply
defaults for missing YAML keys.  The shipped `cfg/config.yaml` contains the full schema;
edit values as needed but do not remove keys.

Key fields to customise:

```yaml
server:
  host: "0.0.0.0"      # Bind address for the HTTP API
  port: 4749            # HTTP API port (4749 when embedded; standalone default is 7878)
  workers: 0            # Tokio worker threads — 0 = auto (num_cpus)

interfaces:
  pgwire:
    enabled: true       # Set false to disable the PostgreSQL wire protocol endpoint

pgwire:
  host: "0.0.0.0"
  port: 5433            # PostgreSQL wire protocol port
  trust: false          # true → skip password auth (dev only, never in production)

paths:
  db_root: dbs          # Directory where Parquet data files are stored.
                        # Created automatically if it does not exist.
                        # Use an absolute path for production.

security:
  admin_user: "admin"   # Provisioned on first startup if no users exist yet.
  admin_password: "admin" # Change before exposing to a network.

logging:
  level: "info"         # trace | debug | info | warn | error
  query_logging: false  # true → logs every executed query
```

All fields can also be overridden with environment variables using the pattern
`REACTIVE__<SECTION>__<KEY>` (double underscore as separator), for example:

```bash
REACTIVE__SERVER__PORT=9000 REACTIVE__PATHS__DB_ROOT=/data/reactive ./single-ion
```

---

### Ion (`cfg/ion.yaml`)

```yaml
server:
  host: 0.0.0.0
  port: 8080
  # Path to ion's compiled static assets (HTML, CSS, JS, i18n).
  # Relative to the working directory, or use an absolute path.
  static_dir: ../ion/static

  # Optional TLS termination (omit for plain HTTP):
  # tls:
  #   cert_path: /etc/certs/server.crt
  #   key_path:  /etc/certs/server.key

reactive:
  # Base URL of the reactive HTTP API — must be reachable from the single-ion process.
  url: http://localhost:4749

auth:
  # HMAC-SHA256 secret for signing session cookies.
  # Change this in production — minimum 64 characters.
  session_secret: "change-me-in-production-please-use-64-or-more-random-bytes!!"
  session_duration_hours: 12

logging:
  filter: "ion=info,tower_http=info"   # tracing-subscriber EnvFilter directive
  format: compact                       # compact | pretty | json
```

Environment variable overrides use `ION_<SECTION>__<KEY>`:

```bash
ION_SERVER__PORT=9090 ION_AUTH__SESSION_SECRET="my-secret" ./single-ion
```

---

### Gluon (`cfg/gluon.yaml`)

```yaml
bind: "0.0.0.0:4747"   # WebSocket / HTTP bind address

# Optional: restrict pub/sub to authenticated sessions validated against reactive.
# auth:
#   reactive_url: http://localhost:4749
```

Environment variable overrides use `GLUON_<KEY>`:

```bash
GLUON_BIND="0.0.0.0:5000" ./single-ion
```

---

### Neutrino (`cfg/neutrino.yaml`)

```yaml
bind: "0.0.0.0:4748"

# Path to the directory containing Jinja2 deployment templates (*.yaml.j2).
# Relative to the working directory, or use an absolute path.
templates_dir: ../neutrino/templates

# Kubernetes namespace for all managed deployments.
namespace: default

# --- Kubernetes authentication (choose one) ---

# Option A: path to a kubeconfig file (takes precedence over B and C).
# kubeconfig: /home/user/.kube/config

# Option B: bearer token + API server URL (for in-cluster or explicit auth).
# api_token:   "eyJhbGci..."
# cluster_url: "https://10.0.0.1:6443"

# Option C: neither set → attempts in-cluster service account (KUBERNETES_SERVICE_HOST).
```

Environment variable overrides use `NEUTRINO_<KEY>`:

```bash
NEUTRINO_NAMESPACE=production NEUTRINO_KUBECONFIG=/etc/k8s/config ./single-ion
```

---

## Environment variables

| Variable | Service | Effect |
|---|---|---|
| `FR_LOG` | single-ion | tracing-subscriber filter for all services (e.g. `info`, `debug`, `ion=debug,reactive=info`) |
| `REACTIVE__*` | reactive | Override any reactive config field (e.g. `REACTIVE__SERVER__PORT=9000`) |
| `ION_*` | ion | Override any ion config field (e.g. `ION_SERVER__PORT=9090`) |
| `ION_LOG` | ion | Shorthand for `ION_LOGGING__FILTER` |
| `GLUON_*` | gluon | Override any gluon config field (e.g. `GLUON_BIND=0.0.0.0:5000`) |
| `NEUTRINO_*` | neutrino | Override any neutrino config field (e.g. `NEUTRINO_NAMESPACE=prod`) |

Environment variables take highest priority, overriding anything in the `cfg/*.yaml` files.

---

## Logging

All four services share a single `tracing-subscriber` instance initialised by the
single-ion binary.  Control verbosity with the `FR_LOG` environment variable:

```bash
# Quiet — warnings and errors only
FR_LOG=warn ./single-ion

# Default — info from all services
FR_LOG=info ./single-ion

# Verbose — debug everything
FR_LOG=debug ./single-ion

# Per-service granularity
FR_LOG="reactive=debug,ion=info,gluon=warn,neutrino=info,tower_http=warn" ./single-ion
```

Log format is plain text (compact).  For JSON logs (e.g. for log aggregation pipelines)
set the environment variable `FR_LOG_JSON=1` — this is a planned feature; until then,
redirect stdout to your aggregator and parse the compact format.

---

## Shutdown

Send `SIGINT` (Ctrl+C) or `SIGTERM` to the process.  The `single-ion` binary catches the
signal, logs a shutdown message, and exits.  Each service's Tokio tasks are cancelled as the
runtime drops.

```bash
# Graceful stop when running in the foreground:
Ctrl+C

# From another terminal:
kill -TERM <pid>
```

If any individual service exits unexpectedly (e.g. port already in use, unrecoverable
error), `single-ion` logs the exit at ERROR level and terminates the whole process.  This
mirrors the behaviour of a process supervisor — a dead service means a restart of the unit.

---

## Moving to distributed deployment

Because each service's `run()` function is identical to what its standalone binary does, you
can move any service back to its own process without code changes:

```bash
# Run reactive and gluon standalone, keep ion + neutrino embedded:
cd reactive  && cargo run --bin reactive_server
cd gluon     && cargo run --bin gluon

# In single-ion/cfg/ion.yaml, reactive.url is already http://localhost:4749 ✓
# In single-ion/cfg/gluon.yaml, set bind to a port that doesn't conflict ✓
cd single-ion && cargo run --bin single-ion
```

For a fully distributed production deployment (e.g. Kubernetes), each service has its own
`Dockerfile`-ready binary and `cfg/` directory in its sub-project.  The `single-ion`
binary is not required in that case — it exists purely as an operational convenience.
