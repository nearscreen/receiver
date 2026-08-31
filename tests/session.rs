//! A fake phone against the real server, in-process: the handshake, one video
//! record and the ways a phone can be turned away — no hardware needed.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

use nearscreen_receiver::net::protocol::{record, Header, FLAG_KEYFRAME, HEADER_SIZE};
use nearscreen_receiver::net::{
    AdmissionFn, AllowAll, Decision, Server, ServerEvent, ServerOptions,
};

const WAIT: Duration = Duration::from_secs(5);

fn options() -> ServerOptions {
    ServerOptions {
        port: 0, // Any free port — the tests must not fight over 9913.
        name: "test-receiver".to_string(),
        heartbeat_timeout: Duration::from_secs(2),
        ..ServerOptions::default()
    }
}

fn next_event(events: &Receiver<ServerEvent>) -> ServerEvent {
    events
        .recv_timeout(WAIT)
        .expect("the server should have reported something by now")
}

/// The phone side of the protocol, written straight from PROTOCOL.md.
struct FakePhone {
    stream: TcpStream,
}

impl FakePhone {
    fn connect(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("cannot reach the receiver");
        stream.set_read_timeout(Some(WAIT)).unwrap();
        Self { stream }
    }

    /// Sends HELLO and returns the receiver's answer.
    fn hello(&mut self, id: &str) -> serde_json::Value {
        let hello = serde_json::json!({
            "v": 1, "id": id, "model": "iPhone12,1", "ios": "26.5", "name": "fake",
            "w": 828, "h": 1792, "codec": "h264", "app": "test",
        });
        let body = serde_json::to_vec(&hello).unwrap();
        self.stream.write_all(b"NSC1").unwrap();
        self.stream
            .write_all(&(body.len() as u32).to_be_bytes())
            .unwrap();
        self.stream.write_all(&body).unwrap();

        let mut magic = [0u8; 4];
        self.stream.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, b"NSS1");
        let mut len = [0u8; 4];
        self.stream.read_exact(&mut len).unwrap();
        let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
        self.stream.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn send(&mut self, rtype: u8, flags: u8, payload: &[u8]) {
        let header = Header::new(rtype, flags, payload.len() as u32, 12_345).encode();
        self.stream.write_all(&header).unwrap();
        self.stream.write_all(payload).unwrap();
    }

    /// Reads one record the receiver sent us.
    fn read_record(&mut self) -> (Header, Vec<u8>) {
        let mut header = [0u8; HEADER_SIZE];
        self.stream.read_exact(&mut header).unwrap();
        let header = Header::parse(&header);
        let mut payload = vec![0u8; header.payload_len as usize];
        self.stream.read_exact(&mut payload).unwrap();
        (header, payload)
    }
}

#[test]
fn a_phone_hands_over_video_and_the_receiver_can_talk_back() {
    let (tx, events) = mpsc::channel();
    let server = Server::start(options(), Arc::new(AllowAll), tx).unwrap();
    let mut phone = FakePhone::connect(server.local_addr().port());

    let reply = phone.hello("VENDOR-ID-A1B2C3D4");
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["name"], "test-receiver");
    assert_eq!(reply["fps"], 30.0);
    assert_eq!(reply["codec"], "h264");

    let handle = match next_event(&events) {
        ServerEvent::SessionStarted { hello, handle, .. } => {
            assert_eq!(hello.short_id(), "A1B2C3D4");
            assert_eq!((hello.w, hello.h), (828, 1792));
            handle
        }
        other => panic!("expected the session to start, got {other:?}"),
    };

    phone.send(
        record::CONFIG,
        0,
        br#"{"codec":"h264","w":828,"h":1792,"fps":30,"bitrate":6000000}"#,
    );
    match next_event(&events) {
        ServerEvent::StreamConfig { config, .. } => {
            assert_eq!(config.w, 828);
            assert_eq!(config.codec, "h264");
        }
        other => panic!("expected the encoder config, got {other:?}"),
    }

    let access_unit = b"\x00\x00\x00\x01\x67fake sps\x00\x00\x00\x01\x65fake idr";
    phone.send(record::VIDEO, FLAG_KEYFRAME, access_unit);
    match next_event(&events) {
        ServerEvent::Video {
            keyframe,
            data,
            pts_us,
        } => {
            assert!(keyframe);
            assert_eq!(data, access_unit);
            assert_eq!(pts_us, 12_345);
        }
        other => panic!("expected a video access unit, got {other:?}"),
    }

    // Heartbeats are absorbed, log lines are passed on.
    phone.send(record::HEARTBEAT, 0, b"");
    phone.send(record::LOG, 0, b"broadcast started\n");
    match next_event(&events) {
        ServerEvent::Log { text, .. } => assert_eq!(text, "broadcast started"),
        other => panic!("expected the phone's log line, got {other:?}"),
    }

    // The one thing we ever ask the phone for.
    handle.request_keyframe().unwrap();
    let (header, payload) = phone.read_record();
    assert_eq!(header.rtype, record::REQUEST_KEYFRAME);
    assert!(payload.is_empty());

    drop(phone);
    match next_event(&events) {
        ServerEvent::SessionEnded { .. } => {}
        other => panic!("expected the session to end, got {other:?}"),
    }
}

#[test]
fn a_second_phone_is_told_the_receiver_is_busy() {
    let (tx, events) = mpsc::channel();
    let server = Server::start(options(), Arc::new(AllowAll), tx).unwrap();
    let port = server.local_addr().port();

    let mut first = FakePhone::connect(port);
    assert_eq!(first.hello("PHONE-ONE")["ok"], true);
    assert!(matches!(
        next_event(&events),
        ServerEvent::SessionStarted { .. }
    ));

    let mut second = FakePhone::connect(port);
    let reply = second.hello("PHONE-TWO");
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["error"], "busy");

    match next_event(&events) {
        ServerEvent::Refused { reason, .. } => assert_eq!(reason, "busy"),
        other => panic!("expected a refusal, got {other:?}"),
    }

    // Once the first phone is gone, the next one gets in.
    drop(first);
    assert!(matches!(
        next_event(&events),
        ServerEvent::SessionEnded { .. }
    ));
    let mut third = FakePhone::connect(port);
    assert_eq!(third.hello("PHONE-THREE")["ok"], true);
}

#[test]
fn a_refused_phone_is_told_why() {
    let (tx, events) = mpsc::channel();
    let admission = AdmissionFn(|_hello: &_, _peer| Decision::Refuse("declined".to_string()));
    let server = Server::start(options(), Arc::new(admission), tx).unwrap();

    let mut phone = FakePhone::connect(server.local_addr().port());
    let reply = phone.hello("NOT-WELCOME");
    assert_eq!(reply["ok"], false);
    assert_eq!(reply["error"], "declined");
    assert!(matches!(next_event(&events), ServerEvent::Refused { .. }));
}

#[test]
fn silence_ends_the_session() {
    let (tx, events) = mpsc::channel();
    let server = Server::start(options(), Arc::new(AllowAll), tx).unwrap();

    let mut phone = FakePhone::connect(server.local_addr().port());
    assert_eq!(phone.hello("QUIET-PHONE")["ok"], true);
    assert!(matches!(
        next_event(&events),
        ServerEvent::SessionStarted { .. }
    ));

    // Say nothing at all: no video, no heartbeat. The 2 s timeout of these
    // tests stands in for the 15 s a real receiver waits.
    match next_event(&events) {
        ServerEvent::SessionEnded { reason, .. } => assert!(
            reason.contains("heartbeat"),
            "the reason should name the missing heartbeat, got {reason:?}"
        ),
        other => panic!("expected the session to time out, got {other:?}"),
    }
}
