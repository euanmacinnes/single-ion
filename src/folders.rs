
use anyhow::Result;
use std::time::Duration;
use tokio::task::JoinError;
use tracing_subscriber::EnvFilter;
use tracing::{info, debug};


/// All files under the `cfg/` tree embedded at compile time (always, including debug builds).
/// On startup, any cfg file that does not exist on disk is written from this embedded copy.
/// Files that already exist are left untouched so that operator customisations survive restarts.
/// Environment variables and command-line args are applied on top by each service's own loader.
#[derive(rust_embed::RustEmbed)]
#[folder = "cfg/"]
struct CfgFiles;

/// Write any embedded cfg file that is absent from `cfg/` relative to the current working
/// directory.  Existing files are never overwritten.
fn ensure_cfg_files() {
    let cfg_dir = std::path::Path::new("cfg");
    for filename in CfgFiles::iter() {
        let dest = cfg_dir.join(filename.as_ref());
        if dest.exists() {
            continue;
        }
        let content = CfgFiles::get(&filename).expect("RustEmbed iter/get are always consistent");
        if let Some(parent) = dest.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!("single-ion: could not create cfg dir {}: {e}", parent.display());
                continue;
            }
        }
        if let Err(e) = std::fs::write(&dest, content.data.as_ref()) {
            tracing::warn!("single-ion: could not write embedded cfg {}: {e}", dest.display());
        } else {
            tracing::info!("single-ion: extracted embedded cfg/{filename}");
        }
    }
}

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
pub fn init_folders() -> (Option<std::path::PathBuf>, Option<std::path::PathBuf>) {

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
    // loaded; the set_path! calls below use absolute paths and are unaffected.
    //
    // Both layouts use an `ion/` subdirectory as the runtime root so that cfg/,
    // dbs/, and extracted scripts never clutter the binary's own directory.
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

    // All runtime state (cfg/, dbs/, files/) lives in an `ion/` subdirectory
    // of the resolved CWD so the binary's own directory stays clean.
    match std::env::current_dir() {
        Ok(cwd) => {
            let ion_dir = cwd.join("ion");
            tracing::info!("single-ion: creating runtime dir {}", ion_dir.display());
            match std::fs::create_dir_all(&ion_dir) {
                Ok(()) => {
                    if let Err(e) = std::env::set_current_dir(&ion_dir) {
                        tracing::warn!("single-ion: could not chdir into {}: {e}", ion_dir.display());
                    }
                }
                Err(e) => {
                    tracing::warn!("single-ion: could not create {}: {e}", ion_dir.display());
                }
            }
        }
        Err(e) => {
            tracing::warn!("single-ion: could not read current directory: {e}");
        }
    }
    tracing::info!("single-ion: runtime CWD = {}", std::env::current_dir().unwrap_or_default().display());

    // Ensure all cfg/*.yaml files exist on disk.  Any that are absent are written from
    // the copies embedded in the binary.  Files already on disk are left untouched so
    // that operator customisations survive restarts.
    ensure_cfg_files();

    (monorepo_root, deployment_root)
}
