//! A real HTTP server for tests, built on `std::net` alone.
//!
//! Everything so far has driven the update flow through a fake [`Fetch`], which
//! proves the logic but never once exercises the code that actually talks to a
//! network. This serves the same artifacts over real TCP so [`HttpFetch`] —
//! request, status handling, streaming to disk — is exercised too.
//!
//! No dependencies, and it binds port 0 so parallel tests cannot collide.
//!
//! [`Fetch`]: tauri_updater_delta_core::client::Fetch
//! [`HttpFetch`]: tauri_plugin_updater_delta::HttpFetch

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// What the server should do with a request.
#[derive(Clone)]
pub enum Route {
    /// Serve these bytes with 200.
    Body(Vec<u8>),
    /// Respond with this status and no useful body.
    Status(u16),
    /// Send headers, then fewer bytes than promised, then hang up.
    ///
    /// Models the interrupted download that a plain 404 does not.
    Truncated(Vec<u8>),
}

type Routes = Arc<Mutex<HashMap<String, Route>>>;

/// A test HTTP server on a background thread.
pub struct TestServer {
    addr: SocketAddr,
    routes: Routes,
    shutdown: Arc<AtomicBool>,
}

impl TestServer {
    /// Start a server on an ephemeral port.
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let thread_routes = Arc::clone(&routes);
        let thread_shutdown = Arc::clone(&shutdown);
        thread::spawn(move || {
            for stream in listener.incoming() {
                if thread_shutdown.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let routes = Arc::clone(&thread_routes);
                thread::spawn(move || {
                    let _ = handle(stream, &routes);
                });
            }
        });

        Self {
            addr,
            routes,
            shutdown,
        }
    }

    /// Full URL for a path served by this instance.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// Serve `bytes` at `path`.
    pub fn serve(&self, path: &str, bytes: Vec<u8>) {
        self.routes
            .lock()
            .expect("routes")
            .insert(path.to_owned(), Route::Body(bytes));
    }

    /// Replace whatever is at `path`.
    pub fn set(&self, path: &str, route: Route) {
        self.routes
            .lock()
            .expect("routes")
            .insert(path.to_owned(), route);
    }

    /// Stop serving `path` entirely — requests get 404.
    pub fn remove(&self, path: &str) {
        self.routes.lock().expect("routes").remove(path);
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Unblock the accept loop so the thread can notice and exit.
        let _ = std::net::TcpStream::connect(self.addr);
    }
}

fn handle(mut stream: TcpStream, routes: &Routes) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    // Drain headers so the client does not see a reset before we reply.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }

    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let route = routes.lock().expect("routes").get(path).cloned();

    match route {
        Some(Route::Body(body)) => {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            stream.write_all(&body)?;
        }
        Some(Route::Status(code)) => {
            write!(
                stream,
                "HTTP/1.1 {code} Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )?;
        }
        Some(Route::Truncated(body)) => {
            // Promise the full length, deliver half, then close.
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            stream.write_all(&body[..body.len() / 2])?;
        }
        None => {
            write!(
                stream,
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )?;
        }
    }

    stream.flush()?;
    Ok(())
}

/// Read a whole file, for building routes from release output.
pub fn read(path: &std::path::Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .expect("open")
        .read_to_end(&mut bytes)
        .expect("read");
    bytes
}
