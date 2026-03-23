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
//! Each service reads its config from the usual locations relative to the
//! process working directory:
//!
//!   cfg/config.yaml      → reactive
//!   cfg/ion.yaml         → ion
//!   cfg/gluon.yaml       → gluon
//!   cfg/neutrino.yaml    → neutrino
//!
//! All per-service environment variable overrides (REACTIVE__*, ION_*,
//! GLUON_*, NEUTRINO_*) continue to work as normal.
//!
//! Set FR_LOG to a tracing-subscriber filter string to control log output,
//! e.g. FR_LOG=debug or FR_LOG=ion=debug,reactive=info.

use anyhow::Result;
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
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .init();

    // Point reactive at the shared scripts directory.  This single variable drives UDF
    // discovery, the DDL installer fallback, and system-view loading — all via
    // find_global_scripts_root() in reactive's system_paths crate.
    // Can be overridden externally with REACTIVE_SCRIPTS_ROOT=<path>.
    if std::env::var("REACTIVE_SCRIPTS_ROOT").is_err() {
        // SAFETY: called before any tasks are spawned, so no concurrent env reads.
        unsafe { std::env::set_var("REACTIVE_SCRIPTS_ROOT", "../reactive/scripts"); }
    }

    // Point each service at its own static assets directory so the /admin pages load
    // correctly when running from the single-ion/ working directory.
    // Each var can be overridden externally (e.g. for Docker deployments).
    if std::env::var("REACTIVE_STATIC_DIR").is_err() {
        unsafe { std::env::set_var("REACTIVE_STATIC_DIR", "../reactive/crates/db_server/static"); }
    }
    if std::env::var("GLUON_STATIC_DIR").is_err() {
        unsafe { std::env::set_var("GLUON_STATIC_DIR", "../gluon/static"); }
    }
    if std::env::var("NEUTRINO_STATIC_DIR").is_err() {
        unsafe { std::env::set_var("NEUTRINO_STATIC_DIR", "../neutrino/static"); }
    }

    tracing::info!("single-ion: loading service configs");

    // Load all configs before spawning so a bad config fails fast and clean.
    let ion_config   = ion_config::load()?;
    let gluon_config = gluon::config::Config::load().unwrap_or_default();
    let neut_config  = neutrino::config::Config::load().unwrap_or_default();
    // Reactive config is loaded inside db_server::run() via db_configs::init_global().

    tracing::info!("single-ion: spawning services");

    // Gluon is spawned first so it has the best chance of being ready before
    // reactive and ion attempt to connect as pub/sub clients.
    let g = tokio::spawn(gluon::run(gluon_config));
    let r = tokio::spawn(db_server::run());
    let i = tokio::spawn(ion::run(ion_config));
    let n = tokio::spawn(neutrino::run(neut_config));

    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("single-ion: shutdown signal received — exiting");
        }
        res = g => {
            let reason = format_exit("gluon", res);
            eprintln!("\n\x1b[1;31m╔══════════════════════════════════════════════════════╗");
            eprintln!("║  single-ion: PROCESS EXITING                         ║");
            eprintln!("║  Service: gluon                                       ║");
            eprintln!("║  {:<53}║", reason);
            eprintln!("╚══════════════════════════════════════════════════════╝\x1b[0m\n");
        }
        res = r => {
            let reason = format_exit("reactive", res);
            eprintln!("\n\x1b[1;31m╔══════════════════════════════════════════════════════╗");
            eprintln!("║  single-ion: PROCESS EXITING                         ║");
            eprintln!("║  Service: reactive                                    ║");
            eprintln!("║  {:<53}║", reason);
            eprintln!("╚══════════════════════════════════════════════════════╝\x1b[0m\n");
        }
        res = i => {
            let reason = format_exit("ion", res);
            eprintln!("\n\x1b[1;31m╔══════════════════════════════════════════════════════╗");
            eprintln!("║  single-ion: PROCESS EXITING                         ║");
            eprintln!("║  Service: ion                                         ║");
            eprintln!("║  {:<53}║", reason);
            eprintln!("╚══════════════════════════════════════════════════════╝\x1b[0m\n");
        }
        res = n => {
            let reason = format_exit("neutrino", res);
            eprintln!("\n\x1b[1;31m╔══════════════════════════════════════════════════════╗");
            eprintln!("║  single-ion: PROCESS EXITING                         ║");
            eprintln!("║  Service: neutrino                                    ║");
            eprintln!("║  {:<53}║", reason);
            eprintln!("╚══════════════════════════════════════════════════════╝\x1b[0m\n");
        }
    }

    Ok(())
}
