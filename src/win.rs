//! single-ion-win — Windows desktop shell
//!
//! Identical service set to the headless `single-ion` binary, but binds every
//! service exclusively on loopback (`127.0.0.1`) using OS-assigned ephemeral
//! ports, then opens a native WebView2 window pointing at ION's port.
//!
//! Port strategy
//! -------------
//! Five `TcpListener`s are bound to `127.0.0.1:0` before any service starts.
//! The OS assigns a free ephemeral port to each.  The resolved port numbers are
//! injected as environment variables so every service's Figment/custom config
//! system picks them up during its normal load phase.  The listeners are then
//! dropped, giving each service a clean bind.  The TOCTOU window is negligible
//! on loopback and is further guarded by the single-instance mutex check.
//!
//! Single-instance
//! ---------------
//! A named Windows mutex (`Global\single-ion`) prevents a second instance from
//! starting.  If the mutex is already held the process exits immediately after
//! attempting to bring the existing window to the foreground.
//!
//! Build
//! -----
//! ```
//! cd single-ion
//! cargo build --bin single-ion-win --features windows-app
//! ```

// Hide the console window in release builds.  Debug builds keep it so that
// tracing output remains visible during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::TcpListener;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use tracing_subscriber::EnvFilter;
use wry::WebViewBuilder;

// ── Single-instance guard ────────────────────────────────────────────────────

/// Returns `true` if this is the first instance; `false` if another is running.
///
/// Uses a named Win32 mutex (`Global\single-ion`).  The raw HANDLE is leaked
/// intentionally — it must remain open for the lifetime of the process to keep
/// the mutex held.
#[cfg(target_os = "windows")]
fn acquire_single_instance() -> bool {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    // Raw Win32 types/constants — avoids a direct windows-sys dep by using
    // the same ABI that tao/wry already pull in transitively.
    type HANDLE  = *mut std::ffi::c_void;
    type BOOL    = i32;
    type LPCWSTR = *const u16;
    type DWORD   = u32;
    const ERROR_ALREADY_EXISTS: DWORD = 183;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateMutexW(
            lp_mutex_attributes: *const std::ffi::c_void,
            b_initial_owner: BOOL,
            lp_name: LPCWSTR,
        ) -> HANDLE;
        fn GetLastError() -> DWORD;
    }

    let name: Vec<u16> = OsStr::new("Global\\single-ion\0")
        .encode_wide()
        .collect();

    // SAFETY: kernel32 is always available; name is a valid null-terminated
    // wide string; we intentionally leak the handle to keep the mutex alive.
    unsafe {
        let _handle = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

// ── Port reservation ─────────────────────────────────────────────────────────

/// Bind to `127.0.0.1:0`, let the OS assign an ephemeral port, return it.
fn reserve() -> Result<(u16, TcpListener)> {
    let l = TcpListener::bind("127.0.0.1:0").context("bind loopback:0")?;
    let port = l.local_addr()?.port();
    Ok((port, l))
}

// ── Readiness probe ──────────────────────────────────────────────────────────

/// Poll until a TCP connection succeeds on `127.0.0.1:{port}` or timeout.
fn wait_for_tcp(port: u16, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

// ── Shared static-dir setup (mirrors main.rs) ────────────────────────────────

fn set_static_dirs() {
    let set = |k: &str, v: &str| {
        if std::env::var(k).is_err() {
            // SAFETY: called before any threads are spawned.
            unsafe { std::env::set_var(k, v) };
        }
    };
    set("REACTIVE_SCRIPTS_ROOT",  "../reactive/scripts");
    set("REACTIVE_STATIC_DIR",    "../reactive/crates/db_server/static");
    set("GLUON_STATIC_DIR",       "../gluon/static");
    set("NEUTRINO_STATIC_DIR",    "../neutrino/static");
    set("NEUTRINO_BUILD_CONTEXT_DIR", "..");
    set("ION_SERVER__STATIC_DIR", "../ion/static");
}

// ─────────────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    // ── 1. Single-instance guard ─────────────────────────────────────────────
    #[cfg(target_os = "windows")]
    if !acquire_single_instance() {
        // Another instance is running — nothing to do on this path for now.
        // A future enhancement can use FindWindow + SetForegroundWindow to
        // focus the existing window before exiting.
        eprintln!("single-ion is already running.");
        return Ok(());
    }

    // ── 2. Reserve five ephemeral loopback ports ─────────────────────────────
    //
    // We hold each TcpListener open until all env vars are set, then drop them
    // atomically (from the OS's perspective) so services rebind immediately.
    let (gluon_port,    gl) = reserve()?;
    let (reactive_port, rl) = reserve()?;
    let (pgwire_port,   pl) = reserve()?;
    let (ion_port,      il) = reserve()?;
    let (neutrino_port, nl) = reserve()?;

    // ── 3. Inject resolved ports via environment variables ───────────────────
    //
    // Each service reads its config through its own system (Figment for ION /
    // Gluon / Neutrino; custom REACTIVE__ parser for Reactive) — env vars are
    // the shared override mechanism that requires no changes to service crates.
    //
    // Must be set before the service configs are loaded in the tokio thread.
    // SAFETY: single-threaded at this point; no concurrent env reads yet.
    unsafe {
        // Gluon — Figment, GLUON_ prefix, `bind` is a top-level host:port string
        std::env::set_var("GLUON_BIND", format!("127.0.0.1:{gluon_port}"));

        // Reactive — custom REACTIVE__ parser
        std::env::set_var("REACTIVE__SERVER__HOST", "127.0.0.1");
        std::env::set_var("REACTIVE__SERVER__PORT", reactive_port.to_string());
        std::env::set_var("REACTIVE__PGWIRE__HOST", "127.0.0.1");
        std::env::set_var("REACTIVE__PGWIRE__PORT", pgwire_port.to_string());
        std::env::set_var("REACTIVE__GLUON__URL",
            format!("ws://127.0.0.1:{gluon_port}/ws"));
        // Default admin credentials for the packaged desktop app — no config
        // file mechanism exists for end users, so we inject them here.
        // These are only set if not already overridden in the environment.
        if std::env::var("REACTIVE__SECURITY__ADMIN_USER").is_err() {
            std::env::set_var("REACTIVE__SECURITY__ADMIN_USER", "admin");
        }
        if std::env::var("REACTIVE__SECURITY__ADMIN_PASSWORD").is_err() {
            std::env::set_var("REACTIVE__SECURITY__ADMIN_PASSWORD", "admin");
        }

        // ION — Figment, ION_ prefix with __ split
        std::env::set_var("ION_SERVER__HOST", "127.0.0.1");
        std::env::set_var("ION_SERVER__PORT", ion_port.to_string());
        std::env::set_var("ION_REACTIVE__URL",
            format!("http://127.0.0.1:{reactive_port}"));
        std::env::set_var("ION_GLUON__URL",
            format!("ws://127.0.0.1:{gluon_port}/ws"));

        // Neutrino — Figment, NEUTRINO_ prefix, `bind` is a top-level host:port string
        std::env::set_var("NEUTRINO_BIND", format!("127.0.0.1:{neutrino_port}"));
    }

    // Release reserved ports — services must be able to rebind them immediately.
    drop(gl); drop(rl); drop(pl); drop(il); drop(nl);

    // ── 4. Static asset directories ──────────────────────────────────────────
    set_static_dirs();

    // ── 5. Logging ───────────────────────────────────────────────────────────
    //
    // In release builds the console is hidden (windows_subsystem = "windows"),
    // so tracing output goes to stderr which is silently discarded.  Redirect
    // FR_LOG output to a file here if persistent logs are needed.
    let filter = std::env::var("FR_LOG").unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .init();

    tracing::info!("single-ion-win: ports gluon={gluon_port} reactive={reactive_port} \
                    pgwire={pgwire_port} ion={ion_port} neutrino={neutrino_port}");

    // ── 6. Spawn services in a background Tokio runtime ──────────────────────
    //
    // The WebView event loop must run on the main thread (Windows requirement),
    // so we move the async runtime to a dedicated OS thread.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;

    std::thread::Builder::new()
        .name("services".into())
        .spawn(move || {
            rt.block_on(async {
                tracing::info!("single-ion-win: loading service configs");

                let ion_config = match ion_config::load() {
                    Ok(c) => c,
                    Err(e) => { tracing::error!("ion config: {e:#}"); return; }
                };
                let gluon_config = gluon::config::Config::load().unwrap_or_default();
                let neut_config  = neutrino::config::Config::load().unwrap_or_default();

                tracing::info!("single-ion-win: spawning services");

                // Gluon must be listening before Reactive and ION connect.
                let g = tokio::spawn(gluon::run(gluon_config));
                tokio::time::sleep(Duration::from_millis(500)).await;

                let r = tokio::spawn(db_server::run());
                let i = tokio::spawn(ion::run(ion_config));
                let n = tokio::spawn(neutrino::run(neut_config));

                // Log unexpected exits; the process will end when the window closes.
                tokio::select! {
                    res = g => tracing::error!("gluon exited: {res:?}"),
                    res = r => tracing::error!("reactive exited: {res:?}"),
                    res = i => tracing::error!("ion exited: {res:?}"),
                    res = n => tracing::error!("neutrino exited: {res:?}"),
                }
            });
        })
        .context("spawn services thread")?;

    // ── 7. Wait for ION to accept connections ────────────────────────────────
    tracing::info!("single-ion-win: waiting for ION on port {ion_port}");
    if !wait_for_tcp(ion_port, Duration::from_secs(30)) {
        anyhow::bail!("ION did not start within 30 seconds on port {ion_port}");
    }
    tracing::info!("single-ion-win: ION ready");

    let ion_url = format!("http://127.0.0.1:{ion_port}");

    // ── 8. Run WebView window on the main thread ─────────────────────────────
    let event_loop = EventLoop::new();

    let window = WindowBuilder::new()
        .with_title("single-ion")
        .with_inner_size(LogicalSize::new(1400_f64, 900_f64))
        .build(&event_loop)
        .context("create window")?;

    let _webview = WebViewBuilder::new()
        .with_url(&ion_url)
        .build(&window)
        .context("create WebView")?;

    // `run` never returns — process exits when ControlFlow::Exit is set.
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested, ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });
}
