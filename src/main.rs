//! single-ion — single embedded binary
//!
//! Runs all four Free Radicals services (reactive, ion, gluon, neutrino) as
//! concurrent async tasks inside one Tokio runtime.  Each service still binds
//! its own port and is independently reachable; the distributed deployment
//! model is fully preserved — the binary is an operational convenience, not
//! an architectural change.
//!
//! Configuration
//! -------------
//! On startup the process changes its working directory to the `single-ion/`
//! subdirectory of the monorepo root (resolved via the exe path).  This means
//! every service's config loader finds its file at the standard CWD-relative
//! path:
//!
//!   cfg/config.yaml      → reactive
//!   cfg/ion.yaml         → ion
//!   cfg/gluon.yaml       → gluon
//!   cfg/neutrino.yaml    → neutrino
//!
//! Static-asset and script paths are set as absolute env vars before the CWD
//! change takes effect, so they are unaffected.
//!
//! All per-service environment variable overrides (REACTIVE__*, ION_*,
//! GLUON_*, NEUTRINO_*) continue to work as normal.
//!
//! Set FR_LOG to a tracing-subscriber filter string to control log output,
//! e.g. FR_LOG=debug or FR_LOG=ion=debug,reactive=info.

use anyhow::Result;
use std::time::Duration;
use tokio::task::JoinError;
use tracing_subscriber::EnvFilter;

fn format_exit(service: &str, res: Result<Result<()>, JoinError>) -> String {
    match res {
        Err(e) if e.is_panic() => format!("{service} panicked: {e}"),
        Err(e) => format!("{service} task error: {e}"),
        Ok(Err(e)) => format!("error: {e}"),
        Ok(Ok(())) => format!("{service} exited cleanly (unexpected)"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Single tracing subscriber for the whole process.
    // Each service's try_init() calls are suppressed once this is set.
    let filter = std::env::var("FR_LOG")
        .unwrap_or_else(|_| "info".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .try_init();

    // Resolve the monorepo root via exe-path traversal so that dev paths work regardless
    // of which directory cargo run is invoked from.  The exe is always inside a `target/`
    // subtree during development; its parent is the monorepo root (freeradicals/).
    // In a distributed build the assets are embedded, so these env vars are unused.
    let monorepo_root: Option<std::path::PathBuf> = std::env::current_exe().ok().and_then(|exe| {
        exe.ancestors()
            .find(|p| p.file_name().map(|n| n == "target").unwrap_or(false))
            .and_then(|t| t.parent())
            .map(|p| p.to_path_buf())
    });

    // Helper: set an env var to an absolute monorepo-relative path if not already set.
    // Falls back to the CWD-relative path (original behaviour) if exe traversal failed.
    macro_rules! set_path {
        ($var:expr, $rel:expr, $fallback:expr) => {
            if std::env::var($var).is_err() {
                let val = monorepo_root
                    .as_ref()
                    .map(|r| r.join($rel).to_string_lossy().into_owned())
                    .unwrap_or_else(|| $fallback.to_string());
                // SAFETY: called before any tasks are spawned, so no concurrent env reads.
                unsafe { std::env::set_var($var, val); }
            }
        };
    }

    // Change CWD to the single-ion/ subdirectory so that every service's
    // config loader finds its `cfg/*.yaml` file via the standard CWD-relative
    // path (e.g. `cfg/config.yaml`, `cfg/ion.yaml`, …).  This must happen
    // before any config is loaded and before the set_path! calls below, which
    // use absolute paths and are therefore unaffected by the CWD change.
    if let Some(ref root) = monorepo_root {
        let single_ion_dir = root.join("single-ion");
        if single_ion_dir.is_dir() {
            std::env::set_current_dir(&single_ion_dir).unwrap_or_else(|e| {
                tracing::warn!("could not chdir to single-ion/: {e}");
            });
        }
    }

    set_path!("REACTIVE_SCRIPTS_ROOT",    "reactive/scripts",                    "../reactive/scripts");
    set_path!("REACTIVE_STATIC_DIR",      "reactive/crates/db_server/static",    "../reactive/crates/db_server/static");
    set_path!("GLUON_STATIC_DIR",         "gluon/static",                        "../gluon/static");
    set_path!("NEUTRINO_STATIC_DIR",      "neutrino/static",                     "../neutrino/static");
    set_path!("ION_SERVER__STATIC_DIR",   "ion/static",                          "../ion/static");

    // The monorepo root is needed to build neutrino-base-standard from source.
    if std::env::var("NEUTRINO_BUILD_CONTEXT_DIR").is_err() {
        let val = monorepo_root
            .as_ref()
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_else(|| "..".to_string());
        unsafe { std::env::set_var("NEUTRINO_BUILD_CONTEXT_DIR", val); }
    }

    tracing::info!("single-ion: loading service configs");

    // Load all configs before spawning so a bad config fails fast and clean.
    let ion_config   = ion_config::load()?;
    let gluon_config = gluon::config::Config::load().unwrap_or_default();

    // Inject the actual Gluon WebSocket URL (derived from gluon's bind address)
    // as env vars so neutrino's config loader picks them up via Figment's Env
    // layer.  This ensures neutrino always connects to the same Gluon instance
    // that single-ion started, regardless of what cfg/neutrino.yaml says.
    {
        let gluon_ws_url = {
            let bind = &gluon_config.bind;
            // Replace 0.0.0.0 with 127.0.0.1 for loopback connections.
            let addr = bind.replace("0.0.0.0", "127.0.0.1");
            format!("ws://{addr}/ws")
        };
        unsafe {
            if std::env::var("NEUTRINO_GLUON__INTERNAL_URL").is_err() {
                std::env::set_var("NEUTRINO_GLUON__INTERNAL_URL", &gluon_ws_url);
            }
            if std::env::var("NEUTRINO_PHOTON__GLUON_URL").is_err() {
                std::env::set_var("NEUTRINO_PHOTON__GLUON_URL", &gluon_ws_url);
            }
        }
    }

    let neut_config  = neutrino::config::Config::load().unwrap_or_default();
    // Reactive config is loaded inside db_server::run() via db_configs::init_global().

    tracing::info!("single-ion: spawning services");

    // Gluon must be listening before Reactive and ION try to connect.
    // Spawn it first, then wait briefly for the WS listener to bind.
    let g = tokio::spawn(gluon::run(gluon_config));
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let r = tokio::spawn(db_server::run());
    let i = tokio::spawn(ion::run(ion_config));
    let n = tokio::spawn(neutrino::run(neut_config));

    let exit_reason = tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            "ctrl-c".to_string()
        }
        res = g => format_exit("gluon", res),
        res = r => format_exit("reactive", res),
        res = i => format_exit("ion", res),
        res = n => format_exit("neutrino", res),
    };

    // ── Graceful shutdown ────────────────────────────────────────────────
    // Signal all services to flush in-flight writes (delta logs, WAL, KV)
    // before the Tokio runtime drops and kills outstanding tasks.
    tracing::info!("single-ion: shutting down ({exit_reason})");
    if let Some(tx) = db_configs::shutdown::get_shutdown_tx() {
        let _ = tx.send(true);
    }
    // Give services time to finish flushing.
    tokio::time::sleep(Duration::from_millis(500)).await;
    tracing::info!("single-ion: shutdown complete");

    Ok(())
}
