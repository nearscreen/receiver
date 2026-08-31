//! Nearscreen wire protocol v1 — framing and JSON messages.
//!
//! The format is described in PROTOCOL.md; this module is a direct
//! transcription of it and does no I/O of its own.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Magic the phone sends before its HELLO.
pub const CLIENT_MAGIC: &[u8; 4] = b"NSC1";
/// Magic the receiver sends before its reply.
pub const SERVER_MAGIC: &[u8; 4] = b"NSS1";
/// Every record starts with a header of this size.
pub const HEADER_SIZE: usize = 16;
/// Port the phone looks for by default.
pub const DEFAULT_PORT: u16 = 9913;
/// Largest record payload we accept — a 1792p keyframe is far below this.
pub const MAX_PAYLOAD: usize = 16 * 1024 * 1024;
/// Largest HELLO JSON we accept.
pub const MAX_HELLO: usize = 64 * 1024;

/// Record types — the `type` byte of the header.
pub mod record {
    /// One Annex-B access unit, client to server.
    pub const VIDEO: u8 = 0x01;
    /// Empty, sent while the screen is static, client to server.
    pub const HEARTBEAT: u8 = 0x02;
    /// JSON [`super::StreamConfig`]: what the encoder produces, client to server.
    pub const CONFIG: u8 = 0x03;
    /// JSON, reserved, client to server.
    pub const STATS: u8 = 0x04;
    /// UTF-8 line for our log, client to server.
    pub const LOG: u8 = 0x05;
    /// Empty, asks the phone for a keyframe, server to client.
    pub const REQUEST_KEYFRAME: u8 = 0x10;
    /// JSON [`super::Params`], applied live, server to client.
    pub const SET_PARAMS: u8 = 0x11;

    /// Human-readable name for logs.
    pub fn name(rtype: u8) -> &'static str {
        match rtype {
            VIDEO => "video",
            HEARTBEAT => "heartbeat",
            CONFIG => "config",
            STATS => "stats",
            LOG => "log",
            REQUEST_KEYFRAME => "request_keyframe",
            SET_PARAMS => "set_params",
            _ => "unknown",
        }
    }
}

/// `flags` bit 0 on a video record: keyframe, carries the parameter sets.
pub const FLAG_KEYFRAME: u8 = 0x01;

/// The 16-byte record header: `u8 type, u8 flags, u16 reserved, u32 len, u64 pts_us`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub rtype: u8,
    pub flags: u8,
    pub payload_len: u32,
    /// The phone's host clock in microseconds.
    pub pts_us: u64,
}

impl Header {
    pub fn new(rtype: u8, flags: u8, payload_len: u32, pts_us: u64) -> Self {
        Self {
            rtype,
            flags,
            payload_len,
            pts_us,
        }
    }

    pub fn parse(bytes: &[u8; HEADER_SIZE]) -> Self {
        let mut len = [0u8; 4];
        len.copy_from_slice(&bytes[4..8]);
        let mut pts = [0u8; 8];
        pts.copy_from_slice(&bytes[8..16]);
        Self {
            rtype: bytes[0],
            flags: bytes[1],
            // bytes[2..4] are reserved and ignored.
            payload_len: u32::from_be_bytes(len),
            pts_us: u64::from_be_bytes(pts),
        }
    }

    pub fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut out = [0u8; HEADER_SIZE];
        out[0] = self.rtype;
        out[1] = self.flags;
        out[4..8].copy_from_slice(&self.payload_len.to_be_bytes());
        out[8..16].copy_from_slice(&self.pts_us.to_be_bytes());
        out
    }

    pub fn is_keyframe(&self) -> bool {
        self.flags & FLAG_KEYFRAME != 0
    }
}

/// Video codec of the elementary stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    pub fn as_str(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "h264" | "avc" => Some(Codec::H264),
            "hevc" | "h265" => Some(Codec::Hevc),
            _ => None,
        }
    }
}

impl fmt::Display for Codec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The phone's opening message. Unknown fields are ignored and missing ones
/// default, so a newer phone never fails the handshake on shape alone.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Hello {
    #[serde(default)]
    pub v: u32,
    /// Stable for this app on that phone — the identity consent remembers.
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub ios: String,
    #[serde(default)]
    pub name: String,
    /// Native screen size in pixels.
    #[serde(default)]
    pub w: u32,
    #[serde(default)]
    pub h: u32,
    /// Codec the phone would use if we express no preference.
    #[serde(default)]
    pub codec: String,
    /// App version.
    #[serde(default)]
    pub app: String,
}

impl Hello {
    /// Short form of the device id, for window titles and dialogs.
    pub fn short_id(&self) -> String {
        let n = self.id.chars().count();
        let tail: String = self.id.chars().skip(n.saturating_sub(8)).collect();
        tail.to_ascii_uppercase()
    }

    /// What to call this phone in the interface.
    pub fn display_name(&self) -> String {
        let name = self.name.trim();
        if name.is_empty() {
            "iPhone".to_string()
        } else {
            name.to_string()
        }
    }
}

/// Our answer to HELLO. Everything but `ok` is optional; whatever we leave out
/// the phone keeps at its own default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HelloReply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyframe_interval_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
}

impl HelloReply {
    /// `ok:false` with a reason — the phone closes and retries with backoff.
    pub fn refused(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            ..Self::default()
        }
    }
}

/// What the encoder actually produces (record `0x03`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamConfig {
    #[serde(default)]
    pub codec: String,
    #[serde(default)]
    pub w: u32,
    #[serde(default)]
    pub h: u32,
    #[serde(default)]
    pub fps: f64,
    #[serde(default)]
    pub bitrate: i64,
}

/// Live encoder settings we can push at any time (record `0x11`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Params {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyframe_interval_s: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let h = Header::new(record::VIDEO, FLAG_KEYFRAME, 123_456, 9_876_543_210);
        let bytes = h.encode();
        assert_eq!(bytes.len(), HEADER_SIZE);
        assert_eq!(Header::parse(&bytes), h);
        assert!(Header::parse(&bytes).is_keyframe());
    }

    #[test]
    fn header_matches_the_wire_layout() {
        // type=1 flags=1 reserved=0 len=2 pts=3, big-endian, as the phone writes it.
        let wire: [u8; HEADER_SIZE] = [1, 1, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 3];
        let h = Header::parse(&wire);
        assert_eq!(h.rtype, record::VIDEO);
        assert_eq!(h.payload_len, 2);
        assert_eq!(h.pts_us, 3);
        assert_eq!(h.encode(), wire);
    }

    #[test]
    fn reserved_bytes_are_ignored_on_read_and_zero_on_write() {
        let mut wire = [0u8; HEADER_SIZE];
        wire[2] = 0xAB;
        wire[3] = 0xCD;
        assert_eq!(Header::parse(&wire).payload_len, 0);
        assert_eq!(
            Header::new(record::HEARTBEAT, 0, 0, 0).encode()[2..4],
            [0, 0]
        );
    }

    #[test]
    fn hello_parses_and_tolerates_extra_fields() {
        let json = r#"{"v":1,"id":"AAAA-BBBB-1234A1B2C3D4","model":"iPhone12,1","ios":"26.5",
                       "name":"Ira iPhone","w":828,"h":1792,"codec":"h264","app":"0.1.0",
                       "future":"whatever"}"#;
        let hello: Hello = serde_json::from_str(json).unwrap();
        assert_eq!(hello.w, 828);
        assert_eq!(hello.short_id(), "A1B2C3D4");
        assert_eq!(hello.display_name(), "Ira iPhone");
        assert_eq!(Codec::parse(&hello.codec), Some(Codec::H264));
    }

    #[test]
    fn short_id_handles_ids_shorter_than_eight() {
        let hello = Hello {
            id: "ab12".to_string(),
            ..Hello::default()
        };
        assert_eq!(hello.short_id(), "AB12");
    }

    #[test]
    fn reply_omits_what_it_does_not_set() {
        let json = serde_json::to_string(&HelloReply::refused("busy")).unwrap();
        assert_eq!(json, r#"{"ok":false,"error":"busy"}"#);
    }

    #[test]
    fn config_record_parses() {
        let cfg: StreamConfig =
            serde_json::from_str(r#"{"codec":"hevc","w":828,"h":1792,"fps":30,"bitrate":6000000}"#)
                .unwrap();
        assert_eq!(Codec::parse(&cfg.codec), Some(Codec::Hevc));
        assert_eq!((cfg.w, cfg.h), (828, 1792));
    }
}
