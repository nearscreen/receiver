//! One phone, one connection: handshake, then records until it goes away.

use std::io::{self, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use log::{debug, info, warn};

use super::protocol::{
    record, Header, Hello, HelloReply, StreamConfig, CLIENT_MAGIC, HEADER_SIZE, MAX_HELLO,
    MAX_PAYLOAD, SERVER_MAGIC,
};
use super::server::{Admission, Decision, ServerEvent, ServerOptions, SessionHandle};

/// Holds "a stream is running" for as long as it lives.
struct BusySlot(Arc<AtomicBool>);

impl BusySlot {
    /// `None` when another phone is already streaming.
    fn acquire(flag: &Arc<AtomicBool>) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| BusySlot(flag.clone()))
    }
}

impl Drop for BusySlot {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub(super) fn run(
    stream: TcpStream,
    peer: SocketAddr,
    options: Arc<ServerOptions>,
    admission: Arc<dyn Admission>,
    events: Sender<ServerEvent>,
    busy: Arc<AtomicBool>,
) {
    if let Err(e) = stream.set_read_timeout(Some(options.heartbeat_timeout)) {
        warn!("[{peer}] cannot set a read timeout: {e}");
        return;
    }
    let writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(e) => {
            warn!("[{peer}] cannot split the connection: {e}");
            return;
        }
    };
    let mut reader = BufReader::with_capacity(64 * 1024, stream);

    let hello = match read_hello(&mut reader) {
        Ok(hello) => hello,
        Err(e) => {
            debug!("[{peer}] no usable handshake: {e}");
            return;
        }
    };
    info!(
        "[{peer}] hello: {} ({} {}, iOS {}, {}x{}, {}, app {})",
        hello.display_name(),
        hello.model,
        hello.short_id(),
        hello.ios,
        hello.w,
        hello.h,
        hello.codec,
        hello.app
    );

    let refuse = |reason: &str, writer: &TcpStream| {
        let mut writer = writer;
        let _ = write_reply(&mut writer, &HelloReply::refused(reason));
        let _ = writer.shutdown(Shutdown::Both);
        let _ = events.send(ServerEvent::Refused {
            peer,
            reason: reason.to_string(),
        });
    };

    match admission.admit(&hello, peer) {
        Decision::Allow => {}
        Decision::Refuse(reason) => {
            info!("[{peer}] refused: {reason}");
            refuse(&reason, &writer);
            return;
        }
        Decision::Ignore => {
            debug!("[{peer}] no answer yet — the phone will retry");
            let _ = writer.shutdown(Shutdown::Both);
            return;
        }
    }

    // One stream at a time: a second phone is told so instead of being ignored.
    let Some(_slot) = BusySlot::acquire(&busy) else {
        info!("[{peer}] refused: another phone is already streaming");
        refuse("busy", &writer);
        return;
    };

    let mut out = &writer;
    if let Err(e) = write_reply(&mut out, &accept_reply(&options)) {
        warn!("[{peer}] cannot answer the handshake: {e}");
        return;
    }
    let handle = SessionHandle::new(peer, writer);
    if events
        .send(ServerEvent::SessionStarted {
            peer,
            hello,
            handle: handle.clone(),
        })
        .is_err()
    {
        return; // Nothing is listening any more — the program is shutting down.
    }

    let reason = pump(&mut reader, peer, &events).unwrap_or_else(|e| e);
    info!("[{peer}] session ended: {reason}");
    handle.disconnect();
    let _ = events.send(ServerEvent::SessionEnded { peer, reason });
}

/// Reads records until the connection ends. `Ok` never happens in practice —
/// both arms carry the human-readable reason the session stopped.
fn pump(
    reader: &mut BufReader<TcpStream>,
    peer: SocketAddr,
    events: &Sender<ServerEvent>,
) -> Result<String, String> {
    loop {
        let mut header_bytes = [0u8; HEADER_SIZE];
        if let Err(e) = reader.read_exact(&mut header_bytes) {
            return Err(describe(e));
        }
        let header = Header::parse(&header_bytes);
        let len = header.payload_len as usize;
        if len > MAX_PAYLOAD {
            return Err(format!(
                "{} record claims {len} bytes — more than we accept",
                record::name(header.rtype)
            ));
        }
        let mut payload = vec![0u8; len];
        if let Err(e) = reader.read_exact(&mut payload) {
            return Err(describe(e));
        }

        let event = match header.rtype {
            record::VIDEO => Some(ServerEvent::Video {
                pts_us: header.pts_us,
                keyframe: header.is_keyframe(),
                data: payload,
            }),
            record::HEARTBEAT => None,
            record::CONFIG => match serde_json::from_slice::<StreamConfig>(&payload) {
                Ok(config) => {
                    info!(
                        "[{peer}] encoder: {} {}x{} {:.0} fps {:.1} Mbit/s",
                        config.codec,
                        config.w,
                        config.h,
                        config.fps,
                        config.bitrate as f64 / 1e6
                    );
                    Some(ServerEvent::StreamConfig { peer, config })
                }
                Err(e) => {
                    warn!("[{peer}] unreadable config record: {e}");
                    None
                }
            },
            record::STATS => serde_json::from_slice(&payload)
                .ok()
                .map(|json| ServerEvent::Stats { peer, json }),
            record::LOG => {
                let text = String::from_utf8_lossy(&payload).trim_end().to_string();
                info!("[{peer}] phone: {text}");
                Some(ServerEvent::Log { peer, text })
            }
            other => {
                debug!("[{peer}] ignoring record type 0x{other:02x} ({len} bytes)");
                None
            }
        };

        if let Some(event) = event {
            if events.send(event).is_err() {
                return Ok("the receiver is shutting down".to_string());
            }
        }
    }
}

/// Turns an I/O error into something worth showing a person.
fn describe(e: io::Error) -> String {
    match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
            "no data and no heartbeat — the phone stopped broadcasting".to_string()
        }
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe => "the phone closed the connection".to_string(),
        _ => e.to_string(),
    }
}

fn accept_reply(options: &ServerOptions) -> HelloReply {
    HelloReply {
        ok: true,
        error: None,
        name: Some(options.name.clone()),
        fps: Some(options.fps),
        bitrate: Some(options.bitrate),
        keyframe_interval_s: Some(options.keyframe_interval_s),
        codec: Some(options.codec.as_str().to_string()),
        scale: options.scale,
    }
}

fn read_hello(reader: &mut BufReader<TcpStream>) -> io::Result<Hello> {
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    if &magic != CLIENT_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected {:?}, got {magic:?}", CLIENT_MAGIC),
        ));
    }
    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes)?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    if len == 0 || len > MAX_HELLO {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("hello of {len} bytes is out of range"),
        ));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(io::Error::other)
}

fn write_reply(writer: &mut impl Write, reply: &HelloReply) -> io::Result<()> {
    let body = serde_json::to_vec(reply).map_err(io::Error::other)?;
    writer.write_all(SERVER_MAGIC)?;
    writer.write_all(&(body.len() as u32).to_be_bytes())?;
    writer.write_all(&body)?;
    writer.flush()
}
