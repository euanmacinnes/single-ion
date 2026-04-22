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

    // ── Working-directory resolution ─────────────────────────────────────────
    //
    // Two layouts are supported:
    //
    //  1. Dev / monorepo  — exe lives inside a `target/` subtree.
    //     monorepo root = first ancestor named `target/`'s parent.
    //     CWD → `<monorepo>/single-ion/`  (config files at cfg/*.yaml).
    //     Asset paths resolved as absolute paths under the monorepo root.
    //
    //  2. Portable deployment — exe is NOT inside a `target/` tree.
    //     Detect by checking whether the exe's own directory contains a `cfg/`
    //     sub-directory (the canonical portable layout: exe + cfg/ side-by-side).
    //     CWD → exe's directory.
    //     Asset paths resolved as absolute paths under that directory.
    //
    // In both cases the absolute paths are set before CWD is changed, so they
    // are unaffected by the chdir.

    let exe_path = std::env::current_exe().ok();

    // Case 1: monorepo dev build.
    let monorepo_root: Option<std::path::PathBuf> = exe_path.as_ref().and_then(|exe| {
        exe.ancestors()
            .find(|p| p.file_name().map(|n| n == "target").unwrap_or(false))
            .and_then(|t| t.parent())
            .map(|p| p.to_path_buf())
    });

    // Case 2: portable deployment (exe-adjacent cfg/).
    let deployment_root: Option<std::path::PathBuf> = if monorepo_root.is_none() {
        exe_path.as_ref()
            .and_then(|exe| exe.parent())
            .map(|p| p.to_path_buf())
            .filter(|d| d.join("cfg").is_dir())
    } else {
        None
    };

    // Helper: set an env var to an absolute path if not already set.
    // Checks the monorepo layout first, then the portable-deployment layout,
    // then falls back to a CWD-relative string (original behaviour).
    macro_rules! set_path {
        ($var:expr, $monorepo_rel:expr, $deploy_rel:expr, $fallback:expr) => {
            if std::env::var($var).is_err() {
                let val = monorepo_root
                    .as_ref()
                    .map(|r| r.join($monorepo_rel).to_string_lossy().into_owned())
                    .or_else(|| deployment_root.as_ref()
                        .map(|d| d.join($deploy_rel).to_string_lossy().into_owned()))
                    .unwrap_or_else(|| $fallback.to_string());
                // SAFETY: called before any tasks are spawned, so no concurrent env reads.
                unsafe { std::env::set_var($var, val); }
            }
        };
    }

    // Change CWD so that every service's config loader finds its `cfg/*.yaml`
    // file at the standard CWD-relative path.  Must happen before config is
    // loaded; the set_path! calls above use absolute paths and are unaffected.
    if let Some(ref root) = monorepo_root {
        let single_ion_dir = root.join("single-ion");
        if single_ion_dir.is_dir() {
            std::env::set_current_dir(&single_ion_dir).unwrap_or_else(|e| {
                tracing::warn!("could not chdir to single-ion/: {e}");
            });
        }
    } else if let Some(ref dir) = deployment_root {
        std::env::set_current_dir(dir).unwrap_or_else(|e| {
            tracing::warn!("could not chdir to deployment dir {}: {e}", dir.display());
        });
        tracing::info!("single-ion: using portable deployment layout at {}", dir.display());
    } else {
        tracing::warn!(
            "single-ion: could not locate a cfg/ directory via exe path or current directory; \
             config files may not be found — place cfg/ alongside the binary for portable use"
        );
    }

    // monorepo_rel: path relative to monorepo root (dev builds)
    // deploy_rel:   path relative to exe's directory (portable builds)
    // fallback:     CWD-relative string used only when neither root is known
    set_path!("REACTIVE_SCRIPTS_ROOT",  "reactive/scripts", "reactive/scripts", "../reactive/scripts");
    set_path!("ION_SERVER__STATIC_DIR", "ion/static",       "ion/static",       "../ion/static");

    // The monorepo root is needed to build neutrino-base-standard from source.
    if std::env::var("NEUTRINO_BUILD_CONTEXT_DIR").is_err() {
        let val = monorepo_root
            .as_ref()
            .map(|r| r.to_string_lossy().into_owned())
            .or_else(|| deployment_root.as_ref().map(|d| d.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "..".to_string());
        unsafe { std::env::set_var("NEUTRINO_BUILD_CONTEXT_DIR", val); }
    }

    // ── Single-ion config defaults ────────────────────────────────────────────
    // These are the single-ion-specific values that differ from each service's
    // standalone defaults.  They are applied only if not already set by the
    // environment or a cfg/*.yaml file.  This makes the binary self-contained:
    // no cfg/ directory is required for basic operation.
    macro_rules! default_env {
        ($var:expr, $val:expr) => {
            if std::env::var($var).is_err() {
                // SAFETY: called before any tasks are spawned.
                unsafe { std::env::set_var($var, $val); }
            }
        };
    }
    // ION → Reactive: use 127.0.0.1 to bypass Windows localhost → ::1 resolution.
    default_env!("ION_REACTIVE__URL", "http://127.0.0.1:4749");
    // Disable connection pooling on loopback — eliminates 3-5 s stall class on Windows.
    default_env!("ION_REACTIVE__HTTP_POOL_MAX_IDLE_PER_HOST", "0");
    // Service token for fast single-REST KV path.
    default_env!("ION_REACTIVE__SERVICE_TOKEN", "single-ion-dev-token");

    // ── Embedded script extraction ────────────────────────────────────────────
    // If REACTIVE_SCRIPTS_ROOT points at a directory that doesn't exist (e.g.
    // the binary was copied to a machine without the source tree), extract the
    // scripts embedded in `db_scripts` to a temp directory and redirect there.
    // The embedded set is always non-empty in both debug and release builds
    // thanks to the `debug-embed` feature on db_scripts.
    let scripts_root_ok = std::env::var("REACTIVE_SCRIPTS_ROOT")
        .map(|p| std::path::Path::new(&p).is_dir())
        .unwrap_or(false);
    if !scripts_root_ok {
        let scripts_tmp = std::env::temp_dir().join("freeradicals-scripts");
        match db_scripts::extract_to(&scripts_tmp) {
            Ok(()) => {
                tracing::info!("single-ion: extracted embedded scripts to {}", scripts_tmp.display());
                unsafe { std::env::set_var("REACTIVE_SCRIPTS_ROOT", &scripts_tmp); }
            }
            Err(e) => {
                tracing::warn!("single-ion: could not extract embedded scripts: {e}");
            }
        }
    }

    tracing::info!("single-ion: loading service configs");

    // Load Gluon config first so we can derive the actual WebSocket URL from its
    // bind address.  single-ion always runs Gluon, so ION must connect to whatever
    // port Gluon actually binds to — not a hardcoded default.
    let gluon_config = gluon::config::Config::load().unwrap_or_default();

    // Derive the actual Gluon WS URL from the bind address (replace 0.0.0.0 with
    // 127.0.0.1 for loopback).  Kept at this scope so it can be patched into
    // ion_config after loading.
    let gluon_ws_url = {
        let addr = gluon_config.bind.replace("0.0.0.0", "127.0.0.1");
        format!("ws://{addr}/ws")
    };

    // Set env vars for Neutrino before its config loads.
    // SAFETY: no tasks spawned yet.
    unsafe {
        if std::env::var("NEUTRINO_GLUON__INTERNAL_URL").is_err() {
            std::env::set_var("NEUTRINO_GLUON__INTERNAL_URL", &gluon_ws_url);
        }
        if std::env::var("NEUTRINO_PHOTON__GLUON_URL").is_err() {
            std::env::set_var("NEUTRINO_PHOTON__GLUON_URL", &gluon_ws_url);
        }
    }

    // Load ION config, then patch Gluon settings directly.
    // single-ion always runs Gluon — force-enable it and point it at the actual
    // bind address regardless of what cfg/ion.yaml says.  Direct struct mutation
    // is simpler and more reliable than env-var injection through Figment.
    let mut ion_config = ion_config::load()?;
    ion_config.gluon.url = gluon_ws_url.clone();
    tracing::info!(gluon_url = %ion_config.gluon.url, "single-ion: Gluon URL configured");

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
