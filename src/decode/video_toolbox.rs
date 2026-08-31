//! The macOS decoder.
//!
//! VideoToolbox decodes on the same hardware the phone encoded with, and it is
//! part of the system, so nothing has to be installed. It wants the stream in
//! a different shape than Windows does: the parameter sets describe the format
//! up front, and every access unit arrives with its lengths in front of it
//! instead of start codes — see [`super::annexb`].

use std::ffi::c_void;
use std::ptr::{self, NonNull};

use anyhow::{anyhow, bail, Context, Result};
use log::{debug, info};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescription,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession,
};

use super::annexb::{self, ParameterSets};
use super::{Decoder, Nv12Frame};
use crate::net::Codec;

/// Two-plane 8-bit 4:2:0 — what a phone's encoder works in, video range.
const NV12_VIDEO_RANGE: u32 = u32::from_be_bytes(*b"420v");
/// The same, full range.
const NV12_FULL_RANGE: u32 = u32::from_be_bytes(*b"420f");

/// Every system on which the receiver can be built has VideoToolbox.
pub fn is_supported(_codec: Codec) -> bool {
    true
}

/// Where the decoder's callback leaves the picture it produced.
#[derive(Default)]
struct Delivery {
    frame: Option<Nv12Frame>,
    failure: Option<String>,
}

pub struct VideoToolboxDecoder {
    codec: Codec,
    parameters: ParameterSets,
    session: Option<CFRetained<VTDecompressionSession>>,
    format: Option<CFRetained<CMFormatDescription>>,
    /// Scratch for the length-prefixed access unit, reused between frames.
    prefixed: Vec<u8>,
}

impl VideoToolboxDecoder {
    pub fn new(codec: Codec, width: u32, height: u32) -> Result<Self> {
        // The real shape arrives with the parameter sets on the first
        // keyframe; what the phone announced is only a hint.
        debug!("VideoToolbox decoder for {codec}, phone says {width}x{height}");
        Ok(Self {
            codec,
            parameters: ParameterSets::default(),
            session: None,
            format: None,
            prefixed: Vec::new(),
        })
    }

    /// Builds the format description and the session from the parameter sets.
    fn start_session(&mut self) -> Result<()> {
        let sets = self.parameters.ordered();
        let pointers: Vec<NonNull<u8>> = sets
            .iter()
            .map(|set| {
                NonNull::new(set.as_ptr() as *mut u8).expect("a parameter set is never empty")
            })
            .collect();
        let sizes: Vec<usize> = sets.iter().map(|set| set.len()).collect();

        let mut format: *const CMFormatDescription = ptr::null();
        let status = unsafe {
            match self.codec {
                Codec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    None,
                    pointers.len(),
                    NonNull::new(pointers.as_ptr() as *mut NonNull<u8>).unwrap(),
                    NonNull::new(sizes.as_ptr() as *mut usize).unwrap(),
                    4,
                    NonNull::from(&mut format),
                ),
                Codec::Hevc => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    None,
                    pointers.len(),
                    NonNull::new(pointers.as_ptr() as *mut NonNull<u8>).unwrap(),
                    NonNull::new(sizes.as_ptr() as *mut usize).unwrap(),
                    4,
                    None,
                    NonNull::from(&mut format),
                ),
            }
        };
        if status != 0 || format.is_null() {
            bail!("the stream's headers are unusable (status {status})");
        }
        let format = unsafe { CFRetained::from_raw(NonNull::new(format as *mut _).unwrap()) };

        let callback = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(on_picture),
            decompressionOutputRefCon: ptr::null_mut(),
        };
        let mut session: *mut VTDecompressionSession = ptr::null_mut();
        let video_format: &CMVideoFormatDescription = &format;
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                video_format,
                None,
                None,
                &callback,
                NonNull::from(&mut session),
            )
        };
        if status != 0 || session.is_null() {
            bail!("VideoToolbox refused this stream (status {status})");
        }
        self.session = Some(unsafe { CFRetained::from_raw(NonNull::new(session).unwrap()) });
        self.format = Some(format);
        info!("VideoToolbox is decoding {}", self.codec);
        Ok(())
    }

    /// Wraps one access unit as something VideoToolbox will accept.
    fn sample(&mut self, access_unit: &[u8]) -> Result<Option<CFRetained<CMSampleBuffer>>> {
        annexb::length_prefixed(access_unit, self.codec, &mut self.prefixed);
        if self.prefixed.is_empty() {
            return Ok(None); // Parameter sets only; nothing to decode.
        }
        let Some(format) = &self.format else {
            return Ok(None);
        };

        let mut block: *mut CMBlockBuffer = ptr::null_mut();
        let status = unsafe {
            CMBlockBuffer::create_with_memory_block(
                None,
                self.prefixed.as_mut_ptr() as *mut c_void,
                self.prefixed.len(),
                // The null allocator means "this memory is not yours to
                // free": the buffer only lives for the decode call.
                objc2_core_foundation::kCFAllocatorNull,
                ptr::null(),
                0,
                self.prefixed.len(),
                0,
                NonNull::from(&mut block),
            )
        };
        if status != 0 || block.is_null() {
            bail!("cannot wrap the frame (status {status})");
        }
        let block = unsafe { CFRetained::from_raw(NonNull::new(block).unwrap()) };

        let mut sample: *mut CMSampleBuffer = ptr::null_mut();
        let sizes = [self.prefixed.len()];
        let status = unsafe {
            CMSampleBuffer::create_ready(
                None,
                Some(&block),
                Some(format),
                1,
                0,
                ptr::null(),
                1,
                sizes.as_ptr(),
                NonNull::from(&mut sample),
            )
        };
        if status != 0 || sample.is_null() {
            bail!("cannot describe the frame (status {status})");
        }
        Ok(Some(unsafe {
            CFRetained::from_raw(NonNull::new(sample).unwrap())
        }))
    }
}

impl Decoder for VideoToolboxDecoder {
    fn decode(&mut self, access_unit: &[u8], pts_us: u64) -> Result<Option<Nv12Frame>> {
        if self.parameters.take_from(access_unit, self.codec) {
            // New headers: the old session cannot decode what follows.
            self.session = None;
            self.format = None;
        }
        if self.session.is_none() {
            if !self.parameters.complete(self.codec) {
                return Ok(None); // Still waiting for a keyframe.
            }
            self.start_session()?;
        }

        let Some(sample) = self.sample(access_unit)? else {
            return Ok(None);
        };
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| anyhow!("the decoder went away"))?;

        // The callback runs before this returns, and leaves the picture here.
        let mut delivery = Delivery::default();
        let mut info = VTDecodeInfoFlags::empty();
        let status = unsafe {
            session.decode_frame(
                &sample,
                VTDecodeFrameFlags::empty(),
                &mut delivery as *mut Delivery as *mut c_void,
                &mut info,
            )
        };
        if status != 0 {
            bail!("VideoToolbox could not decode this frame (status {status})");
        }
        if let Some(failure) = delivery.failure {
            bail!("{failure}");
        }
        Ok(delivery.frame.map(|mut frame| {
            frame.pts_us = pts_us;
            frame
        }))
    }

    fn flush(&mut self) -> Result<()> {
        // Dropping the session is the reset: the next keyframe builds a new
        // one from the parameter sets that come with it.
        self.session = None;
        self.format = None;
        self.parameters = ParameterSets::default();
        Ok(())
    }
}

/// VideoToolbox hands the finished picture here, on the thread that asked.
unsafe extern "C-unwind" fn on_picture(
    _decoder: *mut c_void,
    frame: *mut c_void,
    status: i32,
    _info: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    _presentation: CMTime,
    _duration: CMTime,
) {
    let Some(delivery) = (unsafe { (frame as *mut Delivery).as_mut() }) else {
        return;
    };
    if status != 0 {
        delivery.failure = Some(format!("the decoder reported status {status}"));
        return;
    }
    let Some(image) = (unsafe { image.as_ref() }) else {
        return;
    };
    match unsafe { copy_picture(image) } {
        Ok(frame) => delivery.frame = Some(frame),
        Err(e) => delivery.failure = Some(format!("{e:#}")),
    }
}

/// Copies the decoder's picture out of the system's buffer and into one of
/// ours, so nothing has to stay locked after the call.
unsafe fn copy_picture(image: &CVImageBuffer) -> Result<Nv12Frame> {
    let pixels: &CVPixelBuffer = image;
    let format = CVPixelBufferGetPixelFormatType(pixels);
    if format != NV12_VIDEO_RANGE && format != NV12_FULL_RANGE {
        bail!("the decoder produced a picture in an unexpected format ({format:#x})");
    }

    let status = unsafe { CVPixelBufferLockBaseAddress(pixels, CVPixelBufferLockFlags::ReadOnly) };
    if status != 0 {
        bail!("cannot read the decoded picture (status {status})");
    }

    let width = CVPixelBufferGetWidthOfPlane(pixels, 0) as u32;
    let height = CVPixelBufferGetHeightOfPlane(pixels, 0) as u32;
    let brightness_stride = CVPixelBufferGetBytesPerRowOfPlane(pixels, 0);
    let colour_stride = CVPixelBufferGetBytesPerRowOfPlane(pixels, 1);
    let stride = brightness_stride.max(colour_stride);

    let mut data = vec![0u8; Nv12Frame::buffer_len(stride, height)];
    let copied = (|| -> Result<()> {
        let brightness = CVPixelBufferGetBaseAddressOfPlane(pixels, 0) as *const u8;
        let colour = CVPixelBufferGetBaseAddressOfPlane(pixels, 1) as *const u8;
        if brightness.is_null() || colour.is_null() {
            bail!("the decoded picture has no memory behind it");
        }
        for row in 0..height as usize {
            unsafe {
                ptr::copy_nonoverlapping(
                    brightness.add(row * brightness_stride),
                    data.as_mut_ptr().add(row * stride),
                    brightness_stride.min(stride),
                );
            }
        }
        let colour_rows = CVPixelBufferGetHeightOfPlane(pixels, 1);
        let colour_start = stride * height as usize;
        for row in 0..colour_rows {
            unsafe {
                ptr::copy_nonoverlapping(
                    colour.add(row * colour_stride),
                    data.as_mut_ptr().add(colour_start + row * stride),
                    colour_stride.min(stride),
                );
            }
        }
        Ok(())
    })();
    unsafe { CVPixelBufferUnlockBaseAddress(pixels, CVPixelBufferLockFlags::ReadOnly) };
    copied.context("cannot copy the decoded picture")?;

    Ok(Nv12Frame {
        width,
        height,
        stride,
        data,
        pts_us: 0,
    })
}
