//! Turning the phone's video into pictures with the decoder the operating
//! system already has — Media Foundation on Windows, VideoToolbox on macOS.
//! No external media libraries, nothing for anyone to install.

pub mod nv12;

#[cfg(windows)]
mod media_foundation;

use anyhow::Result;

pub use nv12::Nv12Frame;

use crate::net::Codec;

/// A decoder for one elementary stream.
///
/// Not `Send`: system decoders belong to the thread that made them, so the
/// receiver creates one on its decoding thread and keeps it there.
pub trait Decoder {
    /// Feeds one Annex-B access unit and returns the newest picture that came
    /// out, if any. A decoder needs a keyframe before it can produce anything.
    fn decode(&mut self, access_unit: &[u8], pts_us: u64) -> Result<Option<Nv12Frame>>;

    /// Throws away what is buffered — after a gap, before the next keyframe.
    fn flush(&mut self) -> Result<()>;
}

/// Creates a decoder for a stream of this shape.
pub fn new_decoder(codec: Codec, width: u32, height: u32) -> Result<Box<dyn Decoder>> {
    #[cfg(windows)]
    {
        let decoder = media_foundation::MediaFoundationDecoder::new(codec, width, height)?;
        Ok(Box::new(decoder))
    }
    #[cfg(not(windows))]
    {
        let _ = (codec, width, height);
        anyhow::bail!("this platform has no decoder yet")
    }
}

/// Whether this computer can decode the codec at all. The answer decides what
/// we ask the phone to send: there is no point asking for HEVC on a Windows
/// that never got the HEVC extension.
pub fn is_supported(codec: Codec) -> bool {
    #[cfg(windows)]
    {
        media_foundation::is_supported(codec)
    }
    #[cfg(not(windows))]
    {
        let _ = codec;
        false
    }
}

/// The codec to ask the phone for, given what it says it would send.
pub fn preferred_codec(phone_default: Option<Codec>) -> Codec {
    let wanted = phone_default.unwrap_or(Codec::H264);
    if is_supported(wanted) {
        wanted
    } else if is_supported(Codec::H264) {
        Codec::H264
    } else {
        // Nothing decodes here; ask for the common one and report the failure
        // when the first frame arrives.
        Codec::H264
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_is_asked_for_when_hevc_cannot_be_decoded() {
        // Whatever this machine supports, the choice must be something we can
        // actually name, and must never be HEVC when HEVC is unavailable.
        let chosen = preferred_codec(Some(Codec::Hevc));
        if !is_supported(Codec::Hevc) {
            assert_eq!(chosen, Codec::H264);
        }
    }
}
