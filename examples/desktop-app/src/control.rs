//! Test-only control surface. **Never compiled into a shipping build.**
//!
//! The whole module sits behind `#[cfg(feature = "e2e-control")]` at its
//! declaration in `main.rs`, and the feature is not in `default`. With the
//! feature off there is no listener, no port, and no code — not a disabled
//! branch, but nothing at all in the binary.
//!
//! That is verified rather than asserted: the harness builds the app without
//! the feature and checks both that the marker string is absent from the binary
//! and that nothing is listening. A test-only HTTP surface that ships is a
//! vulnerability added deliberately and then forgotten, so "we remembered to
//! gate it" is not a good enough standard.
//!
//! # Why this exists at all
//!
//! `tauri_plugin_updater::Update` has private fields, so no test can construct
//! one; only `Updater::check()` inside a running app can. Driving the app from
//! outside therefore requires *some* control channel, and the alternatives —
//! AppleScript, accessibility APIs, synthetic UI events — are more fragile,
//! more platform-specific and harder to reason about than a loopback socket.
//!
//! # Shape
//!
//! Plain HTTP on 127.0.0.1, one thread, no dependencies:
//!
//! - `GET /trigger` — run one update, block until it finishes, return the outcome.
//! - `GET /outcome` — the last outcome, or `none`.
//! - `GET /version` — the running app's version.
//! - `GET /cache` — the artifact cache's ACTIVE and PENDING entries.
//! - `GET /reconcile` — re-run launch reconciliation and report what it did.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Runtime};

/// Grep target for the "did this ship?" check. Deliberately distinctive.
const CONTROL_MARKER: &str = "DELTA_E2E_CONTROL_SURFACE_PRESENT";

/// Start the control surface on the port in `DELTA_E2E_CONTROL_PORT`.
///
/// Writes the bound port to `DELTA_E2E_CONTROL_PORT_FILE` if set, so the
/// harness can use port 0 and discover it.
pub fn spawn<R: Runtime>(app: AppHandle<R>) {
    let port: u16 = std::env::var("DELTA_E2E_CONTROL_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);

    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("{CONTROL_MARKER}: could not bind: {e}");
            return;
        }
    };

    if let Ok(addr) = listener.local_addr() {
        eprintln!("{CONTROL_MARKER}: listening on {addr}");
        if let Ok(path) = std::env::var("DELTA_E2E_CONTROL_PORT_FILE") {
            let _ = std::fs::write(path, addr.port().to_string());
        }
    }

    let last_outcome = Arc::new(Mutex::new(String::from("none")));

    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let app = app.clone();
            let last_outcome = Arc::clone(&last_outcome);
            // Serially, not per-connection threads: an update mutates the app
            // bundle, and two at once would be meaningless.
            if let Err(e) = handle(stream, &app, &last_outcome) {
                eprintln!("{CONTROL_MARKER}: request failed: {e}");
            }
        }
    });
}

fn handle<R: Runtime>(
    mut stream: TcpStream,
    app: &AppHandle<R>,
    last_outcome: &Mutex<String>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }

    let path = request_line.split_whitespace().nth(1).unwrap_or("/");

    let body = match path {
        "/trigger" => {
            let result = crate::update::run(app);
            let text = match result {
                Ok(outcome) => format!("ok {outcome}"),
                // Reported, not swallowed: the harness asserts on loud failures
                // too, notably the signature case in DECISIONS #11.
                Err(e) => format!("error {e}"),
            };
            *last_outcome.lock().expect("outcome lock") = text.clone();
            text
        }
        "/outcome" => last_outcome.lock().expect("outcome lock").clone(),
        "/version" => app.package_info().version.to_string(),
        // The cache is state the harness must be able to see: "did the install
        // work" and "did the cache promote" are different questions.
        "/cache" => crate::update::cache_state(app),
        "/reconcile" => crate::update::reconcile_on_launch(app),
        _ => "unknown".to_owned(),
    };

    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}
