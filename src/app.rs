//! single-ion-app — Desktop shell (cross-platform)
//!
//! Identical service set to the headless `single-ion` binary, but binds every
//! service exclusively on loopback (`127.0.0.1`) using OS-assigned ephemeral
//! ports, then opens a native WebView window pointing at ION's port.
//!
//! Platform backends:
//!   - Windows: WebView2
//!   - Linux:   WebKitGTK
//!   - macOS:   WKWebView
//!
//! Port strategy
//! -------------
//! Five `TcpListener`s are bound to `127.0.0.1:0` before any service starts.
//! The OS assigns a free ephemeral port to each.  The resolved port numbers are
//! injected as environment variables so every service's Figment/custom config
//! system picks them up during its normal load phase.  The listeners are then
//! dropped, giving each service a clean bind.  The TOCTOU window is negligible
//! on loopback and is further guarded by the single-instance check.
//!
//! Single-instance
//! ---------------
//! Windows: a named mutex (`Global\single-ion`) prevents a second instance.
//! Linux/macOS: an exclusive `flock` on a lock file in `$XDG_RUNTIME_DIR`
//! (or `/tmp`) prevents a second instance.
//!
//! Build
//! -----
//! ```
//! cd single-ion
//! cargo build --bin single-ion-app --features desktop-app
//! ```

// Hide the console window in release builds on Windows.
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::net::TcpListener;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    window::{Fullscreen, WindowBuilder},
};
use tracing_subscriber::EnvFilter;
use wry::WebViewBuilder;

// ── Single-instance guard (Windows) ──────────────────────────────────────────

/// Returns `true` if this is the first instance; `false` if another is running.
///
/// Uses a named Win32 mutex (`Global\single-ion`).  The raw HANDLE is leaked
/// intentionally — it must remain open for the lifetime of the process to keep
/// the mutex held.
#[cfg(target_os = "windows")]
fn acquire_single_instance() -> bool {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

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

    unsafe {
        let _handle = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

// ── Single-instance guard (Unix) ─────────────────────────────────────────────

/// Returns `true` if this is the first instance; `false` if another is running.
///
/// Uses an exclusive `flock` on a lock file.  The file descriptor is leaked
/// intentionally so the lock is held for the process lifetime.  The OS releases
/// it automatically on exit (including crashes).
#[cfg(target_family = "unix")]
fn acquire_single_instance() -> bool {
    use std::os::unix::io::IntoRawFd;

    let lock_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| "/tmp".into());
    let lock_path = format!("{lock_dir}/single-ion.lock");

    let file = match std::fs::File::create(&lock_path) {
        Ok(f) => f,
        Err(_) => return true, // can't create lock file — allow launch
    };

    // Try to acquire an exclusive, non-blocking lock.
    let fd = file.into_raw_fd(); // leak the fd to hold the lock
    let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    rc == 0
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

// ── Runtime dependency check (Linux) ─────────────────────────────────────────

/// Probe for required shared libraries via `dlopen` and return a list of
/// missing ones.  Called before tao/wry initialise so the user gets a clear
/// message instead of an opaque linker crash.
#[cfg(target_os = "linux")]
fn check_linux_dependencies() -> Vec<&'static str> {
    use std::ffi::CString;

    const REQUIRED: &[(&str, &str)] = &[
        ("libgtk-3.so.0",          "libgtk-3-dev"),
        ("libwebkit2gtk-4.1.so.0", "libwebkit2gtk-4.1-dev"),
    ];

    let mut missing = Vec::new();
    for &(soname, pkg) in REQUIRED {
        let c_name = CString::new(soname).unwrap();
        let handle = unsafe { libc::dlopen(c_name.as_ptr(), libc::RTLD_LAZY) };
        if handle.is_null() {
            missing.push(pkg);
        } else {
            unsafe { libc::dlclose(handle); }
        }
    }
    missing
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
    // ── 0. Check platform dependencies ───────────────────────────────────────
    #[cfg(target_os = "linux")]
    {
        let missing = check_linux_dependencies();
        if !missing.is_empty() {
            eprintln!("\x1b[1;31merror:\x1b[0m single-ion-app requires the following system libraries:\n");
            for pkg in &missing {
                eprintln!("  - {pkg}");
            }
            eprintln!("\nInstall them with:\n");
            eprintln!("  \x1b[1msudo apt install {}\x1b[0m      (Debian/Ubuntu)", missing.join(" "));
            eprintln!("  \x1b[1msudo dnf install {}\x1b[0m      (Fedora)",
                missing.iter().map(|p| p.replace("-dev", "-devel")).collect::<Vec<_>>().join(" "));
            eprintln!();
            anyhow::bail!("missing system dependencies");
        }
    }

    // ── 1. Single-instance guard ─────────────────────────────────────────────
    if !acquire_single_instance() {
        eprintln!("single-ion is already running.");
        return Ok(());
    }

    // ── 2. Reserve five ephemeral loopback ports ─────────────────────────────
    let (gluon_port,    gl) = reserve()?;
    let (reactive_port, rl) = reserve()?;
    let (pgwire_port,   pl) = reserve()?;
    let (ion_port,      il) = reserve()?;
    let (neutrino_port, nl) = reserve()?;

    // ── 3. Inject resolved ports via environment variables ───────────────────
    unsafe {
        // Gluon
        std::env::set_var("GLUON_BIND", format!("127.0.0.1:{gluon_port}"));

        // Reactive
        std::env::set_var("REACTIVE__SERVER__HOST", "127.0.0.1");
        std::env::set_var("REACTIVE__SERVER__PORT", reactive_port.to_string());
        std::env::set_var("REACTIVE__PGWIRE__HOST", "127.0.0.1");
        std::env::set_var("REACTIVE__PGWIRE__PORT", pgwire_port.to_string());
        std::env::set_var("REACTIVE__GLUON__URL",
            format!("ws://127.0.0.1:{gluon_port}/ws"));
        // ION
        std::env::set_var("ION_SERVER__HOST", "127.0.0.1");
        std::env::set_var("ION_SERVER__PORT", ion_port.to_string());
        std::env::set_var("ION_REACTIVE__URL",
            format!("http://127.0.0.1:{reactive_port}"));
        std::env::set_var("ION_GLUON__URL",
            format!("ws://127.0.0.1:{gluon_port}/ws"));

        // Neutrino
        std::env::set_var("NEUTRINO_BIND", format!("127.0.0.1:{neutrino_port}"));
    }

    // Release reserved ports.
    drop(gl); drop(rl); drop(pl); drop(il); drop(nl);

    // ── 4. Static asset directories ──────────────────────────────────────────
    set_static_dirs();

    // ── 5. Logging ───────────────────────────────────────────────────────────
    let filter = std::env::var("FR_LOG").unwrap_or_else(|_| "info".into());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .init();

    tracing::info!("single-ion-app: ports gluon={gluon_port} reactive={reactive_port} \
                    pgwire={pgwire_port} ion={ion_port} neutrino={neutrino_port}");

    // ── 6. Spawn services in a background Tokio runtime ──────────────────────
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;

    std::thread::Builder::new()
        .name("services".into())
        .spawn(move || {
            rt.block_on(async {
                tracing::info!("single-ion-app: loading service configs");

                let ion_config = match ion_config::load() {
                    Ok(c) => c,
                    Err(e) => { tracing::error!("ion config: {e:#}"); return; }
                };
                let gluon_config = gluon::config::Config::load().unwrap_or_default();
                let mut neut_config = neutrino::config::Config::load().unwrap_or_default();
                neut_config.bind          = format!("127.0.0.1:{neutrino_port}");
                neut_config.registry_host = format!("127.0.0.1:{neutrino_port}");

                tracing::info!("single-ion-app: spawning services");

                // Gluon must be listening before Reactive and ION connect.
                let g = tokio::spawn(gluon::run(gluon_config));
                tokio::time::sleep(Duration::from_millis(500)).await;

                let r = tokio::spawn(db_server::run());
                let i = tokio::spawn(ion::run(ion_config));
                let n = tokio::spawn(neutrino::run(neut_config));

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
    tracing::info!("single-ion-app: waiting for ION on port {ion_port}");
    if !wait_for_tcp(ion_port, Duration::from_secs(30)) {
        anyhow::bail!("ION did not start within 30 seconds on port {ion_port}");
    }
    tracing::info!("single-ion-app: ION ready");

    let ion_url = format!("http://127.0.0.1:{ion_port}");

    // ── 8. Run WebView window on the main thread ─────────────────────────────
    //
    // All keyboard shortcuts are handled via JS+IPC rather than tao's
    // WindowEvent::KeyboardInput, because the WebView captures keyboard focus
    // entirely — tao never sees key events while the WebView is active.
    //
    // Keys that only affect window state (fullscreen/quit) are intercepted in
    // the injected script and relayed as IPC messages.  An EventLoopProxy
    // carries quit requests back to the event loop from the IPC handler thread.

    #[derive(Debug)]
    enum AppCmd { Quit }

    let event_loop = EventLoopBuilder::<AppCmd>::with_user_event().build();

    let window = WindowBuilder::new()
        .with_title("single-ion")
        .with_inner_size(LogicalSize::new(1400_f64, 900_f64))
        .build(&event_loop)
        .context("create window")?;
    let proxy = event_loop.create_proxy();
    let window_ref = Arc::new(window);
    let window_ipc = Arc::clone(&window_ref);

    let _webview = WebViewBuilder::new()
        .with_url(&ion_url)
        .with_initialization_script(
            r#"
            document.addEventListener('keydown', function(e) {
                if (e.key === 'F11') {
                    // Toggle OS-level fullscreen; prevent browser's own handling.
                    e.preventDefault();
                    window.ipc.postMessage('toggle-fullscreen');
                } else if (e.key === 'Escape') {
                    // Exit fullscreen if active; don't prevent default so web
                    // content (dialogs, etc.) can still use Escape normally.
                    window.ipc.postMessage('escape');
                } else if ((e.key === 'q' || e.key === 'w') && e.ctrlKey) {
                    // Ctrl+Q / Ctrl+W — quit (Linux/GTK convention).
                    e.preventDefault();
                    window.ipc.postMessage('quit');
                }
            }, true);
            "#,
        )
        .with_ipc_handler(move |msg| match msg.body().as_str() {
            "toggle-fullscreen" => {
                if window_ipc.fullscreen().is_some() {
                    window_ipc.set_fullscreen(None);
                } else {
                    window_ipc.set_fullscreen(Some(Fullscreen::Borderless(None)));
                }
            }
            "escape" => {
                if window_ipc.fullscreen().is_some() {
                    window_ipc.set_fullscreen(None);
                }
            }
            "quit" => {
                let _ = proxy.send_event(AppCmd::Quit);
            }
            _ => {}
        })
        .build(&*window_ref)
        .context("create WebView")?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // Window close button, or Alt+F4 on Windows (handled at OS level).
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                *control_flow = ControlFlow::Exit;
            }
            // Quit request relayed from the IPC handler (Ctrl+Q / Ctrl+W).
            Event::UserEvent(AppCmd::Quit) => {
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
}
