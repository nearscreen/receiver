//! The TCP server: accepts phones and turns each connection into a session.

use std::io::{self, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use log::{debug, warn};

use super::protocol::{record, Codec, Header, Hello, Params, StreamConfig, DEFAULT_PORT};
use super::session;

/// Everything the phone is told at handshake time, plus how patient we are.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub port: u16,
    /// Shown on the phone as the receiver's name.
    pub name: String,
    pub fps: f64,
    pub bitrate: i64,
    pub keyframe_interval_s: f64,
    /// The codec we ask for — we only ask for what we can decode.
    pub codec: Codec,
    /// Fraction of the native screen size to encode, `None` = leave to the phone.
    pub scale: Option<f64>,
    /// Silence longer than this ends the session; the phone heartbeats ~1/s.
    pub heartbeat_timeout: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            name: hostname(),
            fps: 30.0,
            bitrate: 6_000_000,
            keyframe_interval_s: 2.0,
            codec: Codec::H264,
            scale: None,
            heartbeat_timeout: Duration::from_secs(15),
        }
    }
}

/// This computer's name — what the phone shows in its receiver list.
pub fn hostname() -> String {
    let name = gethostname::gethostname().to_string_lossy().to_string();
    if name.trim().is_empty() {
        "Nearscreen receiver".to_string()
    } else {
        name
    }
}

/// What to do with a phone that just said HELLO.
#[derive(Debug, Clone)]
pub enum Decision {
    Allow,
    /// Answer `ok:false` with this reason and close.
    Refuse(String),
    /// Answer nothing and close: used while a consent dialog is open, so the
    /// phone's own retry is what the dialog's answer applies to.
    Ignore,
}

/// Decides whether a phone may stream. Consent lives behind this.
pub trait Admission: Send + Sync + 'static {
    fn admit(&self, hello: &Hello, peer: SocketAddr) -> Decision;
}

/// Accepts every phone.
pub struct AllowAll;

impl Admission for AllowAll {
    fn admit(&self, _hello: &Hello, _peer: SocketAddr) -> Decision {
        Decision::Allow
    }
}

/// Wraps a closure as an [`Admission`].
pub struct AdmissionFn<F>(pub F);

impl<F> Admission for AdmissionFn<F>
where
    F: Fn(&Hello, SocketAddr) -> Decision + Send + Sync + 'static,
{
    fn admit(&self, hello: &Hello, peer: SocketAddr) -> Decision {
        (self.0)(hello, peer)
    }
}

/// Everything the server tells the rest of the program about.
#[derive(Debug)]
pub enum ServerEvent {
    /// A phone was admitted; the stream starts now.
    SessionStarted {
        peer: SocketAddr,
        hello: Hello,
        handle: SessionHandle,
    },
    /// The encoder's actual settings, before the first frame and on every change.
    StreamConfig {
        peer: SocketAddr,
        config: StreamConfig,
    },
    /// One Annex-B access unit.
    Video {
        pts_us: u64,
        keyframe: bool,
        data: Vec<u8>,
    },
    /// A line the phone wants in our log.
    Log { peer: SocketAddr, text: String },
    /// The phone's own statistics (reserved by the protocol).
    Stats {
        peer: SocketAddr,
        json: serde_json::Value,
    },
    /// The session is over — `reason` is fit to show a person.
    SessionEnded { peer: SocketAddr, reason: String },
    /// A phone was turned away before streaming.
    Refused { peer: SocketAddr, reason: String },
}

/// The server-to-phone side of a live session.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    peer: SocketAddr,
    writer: Arc<Mutex<TcpStream>>,
}

impl SessionHandle {
    pub(super) fn new(peer: SocketAddr, stream: TcpStream) -> Self {
        Self {
            peer,
            writer: Arc::new(Mutex::new(stream)),
        }
    }

    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// Ask for a keyframe — after a decoder reset or a gap in the stream.
    pub fn request_keyframe(&self) -> io::Result<()> {
        self.send(record::REQUEST_KEYFRAME, &[])
    }

    /// Change encoder settings on the fly.
    pub fn set_params(&self, params: &Params) -> io::Result<()> {
        let body = serde_json::to_vec(params).map_err(io::Error::other)?;
        self.send(record::SET_PARAMS, &body)
    }

    /// End the session from our side.
    pub fn disconnect(&self) {
        let stream = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let _ = stream.shutdown(Shutdown::Both);
    }

    fn send(&self, rtype: u8, payload: &[u8]) -> io::Result<()> {
        let header = Header::new(rtype, 0, payload.len() as u32, 0).encode();
        let mut stream = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        stream.write_all(&header)?;
        if !payload.is_empty() {
            stream.write_all(payload)?;
        }
        stream.flush()
    }
}

/// A listening receiver. Dropping it stops the server.
pub struct Server {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    /// Binds the port and starts accepting. Fails if the port is taken.
    pub fn start(
        options: ServerOptions,
        admission: Arc<dyn Admission>,
        events: Sender<ServerEvent>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", options.port)).with_context(|| {
            format!(
                "cannot listen on port {} — another program is probably using it",
                options.port
            )
        })?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let options = Arc::new(options);
        let busy = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            thread::Builder::new()
                .name("nearscreen-accept".to_string())
                .spawn(move || accept_loop(listener, options, admission, events, busy, stop))?
        };
        Ok(Self {
            addr,
            stop,
            thread: Some(thread),
        })
    }

    /// The address actually bound — the port is resolved when `port` was 0.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stops accepting and waits for the accept thread. Live sessions end when
    /// their phone disconnects.
    pub fn stop(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept with a connection that goes nowhere.
        let wake = SocketAddr::from(([127, 0, 0, 1], self.addr.port()));
        if let Ok(stream) = TcpStream::connect_timeout(&wake, Duration::from_millis(500)) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        let _ = thread.join();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(
    listener: TcpListener,
    options: Arc<ServerOptions>,
    admission: Arc<dyn Admission>,
    events: Sender<ServerEvent>,
    busy: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
) {
    for incoming in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let stream = match incoming {
            Ok(stream) => stream,
            Err(e) => {
                warn!("accept failed: {e}");
                continue;
            }
        };
        let peer = match stream.peer_addr() {
            Ok(peer) => peer,
            Err(e) => {
                debug!("connection without a peer address: {e}");
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let options = options.clone();
        let admission = admission.clone();
        let events = events.clone();
        let busy = busy.clone();
        let spawned = thread::Builder::new()
            .name(format!("nearscreen-session-{peer}"))
            .spawn(move || session::run(stream, peer, options, admission, events, busy));
        if let Err(e) = spawned {
            warn!("cannot start a session thread for {peer}: {e}");
        }
    }
    debug!("accept loop stopped");
}
