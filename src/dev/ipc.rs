//! The dev-host control channel: a Unix-domain socket speaking newline-delimited
//! JSON (JSON-Lines), request/response, thread-per-connection (SPEC D §2).
//!
//! FOUNDATION-OWNED wire types + the client `connect`/`call`/`stream` helpers + the
//! `serve` accept-loop scaffold. The daemon-core agent supplies the request `Handler`
//! (the trait object `serve` drives); this file freezes the [`Request`]/[`Response`]
//! enums, the framing, and the protocol-version check so every other module compiles
//! against a stable wire form. No async runtime — std threads cover the single-daemon,
//! few-clients load and match the rest of the codebase.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{CmdResult, ErrorCode, RkError};

use super::{DEV_PROTOCOL_VERSION, DevEvent, ExtStatus, HostState};

fn default_protocol_version() -> u32 {
    DEV_PROTOCOL_VERSION
}

/// A request from a client to the daemon. The wire form is `{"v":1,"type":"…",…}`
/// (one JSON object per line). `v` defaults to the current protocol version and is
/// checked on receipt; a mismatch is answered with `RK0308`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    #[serde(default = "default_protocol_version")]
    pub v: u32,
    #[serde(flatten)]
    pub request: Request,
}

impl RequestEnvelope {
    pub fn new(request: Request) -> Self {
        Self {
            v: DEV_PROTOCOL_VERSION,
            request,
        }
    }
}

/// The request variants (SPEC D §2 message table).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Health probe.
    Ping,
    /// Full daemon + per-extension status snapshot.
    Status,
    /// Force a whole-host reload now (optionally scoped to `only`); `strict` makes a
    /// pre-filtered skip fatal.
    Reload {
        #[serde(default)]
        only: Option<Vec<String>>,
        #[serde(default)]
        strict: bool,
    },
    /// Set the transient working set (`None` = the full enabled set), then reload.
    SetWorkingSet {
        #[serde(default)]
        names: Option<Vec<String>>,
    },
    /// Toggle the node inspector; restarts the host child when toggling on a running
    /// host (§7 restart-with-announcement).
    SetInspect {
        enable: bool,
        host: String,
        port: u16,
    },
    /// Tail/filter the per-extension log sink; `follow` streams until `StopStream`.
    Logs {
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        follow: bool,
        #[serde(default)]
        since_ms: Option<u64>,
        #[serde(default)]
        level: Option<String>,
        #[serde(default)]
        raw: bool,
    },
    /// Cancel an in-flight `Logs` stream on this connection.
    StopStream,
    /// Subscribe this connection to unsolicited `Event` broadcasts (the watch UI).
    Subscribe,
    /// Stop the daemon: killpg the host group, unlink sock+pidfile, exit.
    Shutdown,
}

/// A response from the daemon. Streaming requests (`Logs{follow:true}`, `Subscribe`)
/// produce many lines; everything else is a single response line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    #[serde(default = "default_protocol_version")]
    pub v: u32,
    #[serde(flatten)]
    pub response: Response,
}

impl ResponseEnvelope {
    pub fn new(response: Response) -> Self {
        Self {
            v: DEV_PROTOCOL_VERSION,
            response,
        }
    }
}

/// The inspector state reported in `Status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectorState {
    pub active: bool,
    pub host: String,
    pub port: u16,
}

/// One failed extension in a reload result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedExt {
    pub name: String,
    pub error: String,
}

/// One skipped extension in a reload result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedExt {
    pub name: String,
    pub reason: String,
}

/// The response variants (SPEC D §2 message table).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Pong {
        pid: i32,
        pgid: i32,
        daemon_version: String,
        protocol_v: u32,
    },
    Status {
        host: HostState,
        extensions: Vec<ExtStatus>,
        live_app: String,
        host_module: String,
        eh_node: String,
        dev_mode: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inspector: Option<InspectorState>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reload_ms_last: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reload_ms_p50: Option<u64>,
    },
    ReloadResult {
        ok: bool,
        reloaded: Vec<String>,
        failed: Vec<FailedExt>,
        skipped: Vec<SkippedExt>,
        reload_ms: u64,
        host_state: HostState,
    },
    Ack {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_set: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restarted: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inspector: Option<InspectorState>,
    },
    /// One streamed log line (for `Logs{follow:…}`).
    LogLine {
        ts_ms: u64,
        level: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ext: Option<String>,
        kind: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mapped: Option<super::SourceLoc>,
    },
    /// Terminates a non-following `Logs` response.
    LogEnd,
    /// An unsolicited daemon→client event on a subscribed connection.
    Event(DevEvent),
    Error {
        code: String,
        msg: String,
    },
}

/// A blocking JSON-Lines client over the daemon's control socket.
pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl Client {
    /// Connect to the daemon at `sock`. Absent/refused → `RK0309 NoDaemon` (the
    /// command should tell the user to run `rackabel dev`).
    pub fn connect(sock: &Path) -> CmdResult<Client> {
        let stream = UnixStream::connect(sock).map_err(|e| {
            RkError::of(
                ErrorCode::NoDaemon,
                "no dev host is running",
                "start it with `rackabel dev` (or `rackabel dev start`), then retry",
            )
            .at(sock.display().to_string())
            .raw(e.into())
        })?;
        let reader = BufReader::new(stream.try_clone().map_err(io_err)?);
        Ok(Client {
            reader,
            writer: stream,
        })
    }

    /// One request → one response round-trip.
    pub fn call(&mut self, req: Request) -> CmdResult<Response> {
        self.send(req)?;
        match self.recv()? {
            Some(resp) => Ok(resp),
            None => Err(RkError::of(
                ErrorCode::NoDaemon,
                "the dev host closed the connection without a reply",
                "restart it with `rackabel dev stop && rackabel dev`",
            )),
        }
    }

    /// A streaming request whose iterator yields response lines until the daemon
    /// sends `LogEnd`/closes, or the caller drops it. The caller is responsible for
    /// sending `StopStream` (on a separate writer clone) to cancel a `follow` stream.
    pub fn stream(
        &mut self,
        req: Request,
    ) -> CmdResult<impl Iterator<Item = CmdResult<Response>> + '_> {
        self.send(req)?;
        Ok(StreamIter { client: self })
    }

    /// Clone the underlying socket for an out-of-band write (e.g. `StopStream` while a
    /// stream iterator is being consumed).
    pub fn writer_clone(&self) -> CmdResult<UnixStream> {
        self.writer.try_clone().map_err(io_err)
    }

    fn send(&mut self, req: Request) -> CmdResult<()> {
        let line = serde_json::to_string(&RequestEnvelope::new(req)).map_err(json_err)?;
        self.writer.write_all(line.as_bytes()).map_err(io_err)?;
        self.writer.write_all(b"\n").map_err(io_err)?;
        self.writer.flush().map_err(io_err)?;
        Ok(())
    }

    fn recv(&mut self) -> CmdResult<Option<Response>> {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).map_err(io_err)?;
        if n == 0 {
            return Ok(None);
        }
        let env: ResponseEnvelope = serde_json::from_str(line.trim_end()).map_err(json_err)?;
        check_version(env.v)?;
        Ok(Some(env.response))
    }
}

struct StreamIter<'a> {
    client: &'a mut Client,
}

impl Iterator for StreamIter<'_> {
    type Item = CmdResult<Response>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.client.recv() {
            Ok(Some(Response::LogEnd)) => None,
            Ok(Some(resp)) => Some(Ok(resp)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

/// A sink the daemon's `Handler` writes responses to. A streaming handler calls
/// `send` many times then returns; a one-shot handler calls it once. The daemon-core
/// agent implements `Handler` and uses this to push `Event`/`LogLine` lines.
pub trait ResponseSink: Send {
    /// Write one response line to the connection.
    fn send(&mut self, resp: Response) -> CmdResult<()>;
    /// Whether the peer asked to stop an in-flight stream (best-effort).
    fn stop_requested(&self) -> bool {
        false
    }
    /// A per-connection id, so the handler can tie session-scoped state (the transient
    /// working set) to the connection that set it and reset it when that connection
    /// drops. `0` = an anonymous/one-shot connection (the default sink).
    fn conn_id(&self) -> u64 {
        0
    }
}

/// The daemon's request handler. Implemented by daemon-core (over `host.rs`/
/// `daemon.rs` state). `serve` drives it once per received request line.
pub trait Handler: Send + Sync {
    fn handle(&self, req: Request, conn: &mut dyn ResponseSink);
}

/// A `ResponseSink` over a raw `UnixStream` (one connection). Used by `serve`.
struct StreamSink {
    writer: UnixStream,
}

impl ResponseSink for StreamSink {
    fn send(&mut self, resp: Response) -> CmdResult<()> {
        let line = serde_json::to_string(&ResponseEnvelope::new(resp)).map_err(json_err)?;
        self.writer.write_all(line.as_bytes()).map_err(io_err)?;
        self.writer.write_all(b"\n").map_err(io_err)?;
        self.writer.flush().map_err(io_err)?;
        Ok(())
    }
}

/// The accept loop: one thread per connection, each line decoded into a [`Request`]
/// and dispatched to `handler`. A protocol-version mismatch is answered with an
/// `RK0308` error line and the connection is closed. This is the scaffold; the
/// daemon-core agent owns the `Handler` body and the shutdown wiring that breaks the
/// loop.
pub fn serve(listener: UnixListener, handler: Arc<dyn Handler>) -> CmdResult<()> {
    for conn in listener.incoming() {
        let stream = match conn {
            Ok(s) => s,
            Err(_) => continue,
        };
        let handler = Arc::clone(&handler);
        std::thread::spawn(move || {
            let _ = handle_connection(stream, handler);
        });
    }
    Ok(())
}

/// The maximum bytes one request line may occupy before the connection is rejected —
/// a small JSON object; anything larger is malformed or hostile (an unterminated stream
/// would otherwise buffer unboundedly, finding #12). Mirrors the daemon's own cap.
const MAX_REQUEST_LINE: u64 = 64 * 1024;

fn handle_connection(stream: UnixStream, handler: Arc<dyn Handler>) -> CmdResult<()> {
    let writer = stream.try_clone().map_err(io_err)?;
    let mut sink = StreamSink { writer };
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        let mut limited = (&mut reader).take(MAX_REQUEST_LINE + 1);
        let n = match limited.read_line(&mut line) {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 {
            break;
        }
        if n as u64 > MAX_REQUEST_LINE && !line.ends_with('\n') {
            let _ = sink.send(Response::Error {
                code: ErrorCode::ProtocolMismatch.as_str().to_string(),
                msg: "request line too large".to_string(),
            });
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<RequestEnvelope>(&line) {
            Ok(env) => {
                if env.v != DEV_PROTOCOL_VERSION {
                    let _ = sink.send(protocol_mismatch_response());
                    break;
                }
                handler.handle(env.request, &mut sink);
            }
            Err(_) => {
                let _ = sink.send(Response::Error {
                    code: ErrorCode::ProtocolMismatch.as_str().to_string(),
                    msg: "malformed request".to_string(),
                });
            }
        }
    }
    Ok(())
}

fn protocol_mismatch_response() -> Response {
    Response::Error {
        code: ErrorCode::ProtocolMismatch.as_str().to_string(),
        msg:
            "protocol version mismatch — restart the dev host (`rackabel dev stop && rackabel dev`)"
                .to_string(),
    }
}

/// Map an unexpected daemon-side protocol version to the framed `RK0308` error.
fn check_version(v: u32) -> CmdResult<()> {
    if v == DEV_PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(RkError::of(
            ErrorCode::ProtocolMismatch,
            "the dev host speaks a different control protocol",
            "restart the dev host: `rackabel dev stop && rackabel dev`",
        )
        .at(format!(
            "daemon protocol v{v}, this build v{DEV_PROTOCOL_VERSION}"
        )))
    }
}

fn io_err(e: std::io::Error) -> RkError {
    RkError::of(
        ErrorCode::NoDaemon,
        "lost the connection to the dev host",
        "restart it with `rackabel dev stop && rackabel dev`",
    )
    .raw(e.into())
}

fn json_err(e: serde_json::Error) -> RkError {
    RkError::of(
        ErrorCode::ProtocolMismatch,
        "could not decode a dev host message",
        "restart the dev host: `rackabel dev stop && rackabel dev`",
    )
    .raw(e.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn request_round_trips_on_the_wire() {
        let env = RequestEnvelope::new(Request::Reload {
            only: Some(vec!["foo".into()]),
            strict: true,
        });
        let line = serde_json::to_string(&env).unwrap();
        assert!(line.contains("\"v\":1"));
        assert!(line.contains("\"type\":\"reload\""));
        let back: RequestEnvelope = serde_json::from_str(&line).unwrap();
        assert!(matches!(
            back.request,
            Request::Reload {
                strict: true,
                only: Some(_)
            }
        ));
    }

    #[test]
    fn missing_v_defaults_to_current() {
        let back: RequestEnvelope = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        assert_eq!(back.v, DEV_PROTOCOL_VERSION);
        assert!(matches!(back.request, Request::Ping));
    }

    #[test]
    fn version_check_rejects_other() {
        assert!(check_version(DEV_PROTOCOL_VERSION).is_ok());
        let e = check_version(999).unwrap_err();
        assert_eq!(e.code, ErrorCode::ProtocolMismatch);
    }

    /// A minimal in-process handler proves the `serve`/`Client` round-trip and that
    /// the protocol-mismatch path is wired — this is the foundation contract the
    /// daemon-core agent's real `Handler` slots into.
    #[test]
    fn serve_and_client_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");

        struct PingHandler {
            calls: Arc<Mutex<u32>>,
        }
        impl Handler for PingHandler {
            fn handle(&self, req: Request, conn: &mut dyn ResponseSink) {
                *self.calls.lock().unwrap() += 1;
                match req {
                    Request::Ping => {
                        let _ = conn.send(Response::Pong {
                            pid: 1,
                            pgid: 1,
                            daemon_version: "test".into(),
                            protocol_v: DEV_PROTOCOL_VERSION,
                        });
                    }
                    _ => {
                        let _ = conn.send(Response::Error {
                            code: "RK0000".into(),
                            msg: "unexpected".into(),
                        });
                    }
                }
            }
        }

        let listener = UnixListener::bind(&sock).unwrap();
        let calls = Arc::new(Mutex::new(0));
        let handler = Arc::new(PingHandler {
            calls: Arc::clone(&calls),
        });
        let server = std::thread::spawn(move || {
            // Accept exactly one connection for the test, then return.
            if let Ok((stream, _)) = listener.accept() {
                let _ = handle_connection(stream, handler);
            }
        });

        let mut client = Client::connect(&sock).unwrap();
        let resp = client.call(Request::Ping).unwrap();
        assert!(matches!(resp, Response::Pong { pid: 1, .. }));
        drop(client);
        server.join().unwrap();
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[test]
    fn server_rejects_bad_protocol_version() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        struct H;
        impl Handler for H {
            fn handle(&self, _req: Request, conn: &mut dyn ResponseSink) {
                let _ = conn.send(Response::LogEnd);
            }
        }
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let _ = handle_connection(stream, Arc::new(H));
            }
        });
        // Hand-write a request with a bad version.
        let mut stream = UnixStream::connect(&sock).unwrap();
        stream.write_all(br#"{"v":999,"type":"ping"}"#).unwrap();
        stream.write_all(b"\n").unwrap();
        stream.flush().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let env: ResponseEnvelope = serde_json::from_str(line.trim_end()).unwrap();
        match env.response {
            Response::Error { code, .. } => {
                assert_eq!(code, ErrorCode::ProtocolMismatch.as_str());
            }
            other => panic!("expected error, got {other:?}"),
        }
        server.join().unwrap();
    }
}
