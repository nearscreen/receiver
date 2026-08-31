//! The Windows decoder.
//!
//! Media Foundation ships an H.264 decoder with the system; HEVC comes from
//! the "HEVC Video Extensions", which a given machine may or may not have.
//! Both are found the same way — by asking Media Foundation for a transform
//! that turns this codec into NV12 — so a missing HEVC decoder is simply
//! "not supported here" rather than a failure at the first frame.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr;
use std::sync::Once;

use anyhow::{anyhow, bail, Context, Result};
use log::{debug, info, warn};
use windows::core::GUID;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

use super::{Decoder, Nv12Frame};
use crate::net::Codec;

/// Media Foundation counts time in 100-nanosecond units.
const HNS_PER_MICROSECOND: i64 = 10;

/// Progressive video — the phone never sends anything else.
const PROGRESSIVE: u32 = 2;

static MEDIA_FOUNDATION: Once = Once::new();

/// Starts the platform once per process, and COM once per thread.
fn start_media_foundation() {
    unsafe {
        // Decoders are used from one thread each; joining the multi-threaded
        // apartment is what Media Foundation expects. Repeat calls are fine.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    MEDIA_FOUNDATION.call_once(|| unsafe {
        if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) {
            warn!("Media Foundation did not start: {e}");
        }
    });
}

fn subtype_of(codec: Codec) -> GUID {
    match codec {
        Codec::H264 => MFVideoFormat_H264,
        Codec::Hevc => MFVideoFormat_HEVC,
    }
}

/// Whether this computer has a decoder for the codec.
pub fn is_supported(codec: Codec) -> bool {
    match find_decoder(codec) {
        Ok(_) => true,
        Err(e) => {
            debug!("no {codec} decoder here: {e:#}");
            false
        }
    }
}

/// Asks Media Foundation for something that decodes `codec` into NV12.
fn find_decoder(codec: Codec) -> Result<IMFTransform> {
    start_media_foundation();

    let input = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: subtype_of(codec),
    };
    let output = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_NV12,
    };

    unsafe {
        let mut activates: *mut Option<IMFActivate> = ptr::null_mut();
        let mut count: u32 = 0;
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut activates,
            &mut count,
        )
        .with_context(|| format!("cannot look for a {codec} decoder"))?;

        // Take ownership of every entry, then free the array itself; the ones
        // we do not use are released when they go out of scope.
        let found: Vec<Option<IMFActivate>> = (0..count as usize)
            .map(|i| ptr::read(activates.add(i)))
            .collect();
        CoTaskMemFree(Some(activates as *const c_void));

        let chosen = found
            .into_iter()
            .flatten()
            .next()
            .ok_or_else(|| anyhow!("this computer has no {codec} decoder"))?;
        chosen
            .ActivateObject::<IMFTransform>()
            .with_context(|| format!("cannot start the {codec} decoder"))
    }
}

/// The shape of what comes out of the decoder.
struct OutputFormat {
    /// The picture itself.
    width: u32,
    height: u32,
    /// What the decoder actually fills: the picture rounded up to whole
    /// macroblocks. The colour plane starts after *these* rows.
    coded_height: u32,
    /// Bytes per row — decoders pad rows, so this is usually wider than the picture.
    stride: usize,
    buffer_len: usize,
    /// Some decoders hand out their own buffers; others expect us to bring one.
    decoder_allocates: bool,
}

/// One step of draining the decoder.
enum Output {
    Frame(Box<Nv12Frame>),
    NeedMoreInput,
    FormatChanged,
}

pub struct MediaFoundationDecoder {
    transform: IMFTransform,
    output: OutputFormat,
    /// Reused between frames so a stream does not allocate per picture.
    spare_output: Option<IMFSample>,
}

impl MediaFoundationDecoder {
    pub fn new(codec: Codec, width: u32, height: u32) -> Result<Self> {
        let transform = find_decoder(codec)?;
        unsafe {
            if let Ok(attributes) = transform.GetAttributes() {
                // Ask for pictures as soon as they are ready rather than in
                // the largest, most efficient batches.
                let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            }

            let input_type = MFCreateMediaType().context("cannot describe the incoming video")?;
            input_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input_type.SetGUID(&MF_MT_SUBTYPE, &subtype_of(codec))?;
            input_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_size(width, height))?;
            input_type.SetUINT32(&MF_MT_INTERLACE_MODE, PROGRESSIVE)?;
            transform
                .SetInputType(0, &input_type, 0)
                .with_context(|| format!("the {codec} decoder rejected {width}x{height}"))?;

            let output = configure_output(&transform)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            info!(
                "{codec} decoder ready for {}x{} (stride {})",
                output.width, output.height, output.stride
            );
            Ok(Self {
                transform,
                output,
                spare_output: None,
            })
        }
    }

    /// Takes everything the decoder is ready to hand over, keeping the newest
    /// picture — on a screen mirror, an older one has no value.
    fn drain(&mut self) -> Result<Option<Box<Nv12Frame>>> {
        let mut newest = None;
        loop {
            match self.process_output()? {
                Output::Frame(frame) => newest = Some(frame),
                Output::FormatChanged => continue,
                Output::NeedMoreInput => break,
            }
        }
        Ok(newest)
    }

    fn process_output(&mut self) -> Result<Output> {
        let ours = if self.output.decoder_allocates {
            None
        } else {
            Some(self.spare_sample()?)
        };

        unsafe {
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(ours),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let result = self.transform.ProcessOutput(0, &mut buffers, &mut status);

            // Whatever happened, take back what is in the struct so nothing leaks.
            let produced = ManuallyDrop::take(&mut buffers[0].pSample);
            let _events = ManuallyDrop::take(&mut buffers[0].pEvents);

            match result {
                Ok(()) => {
                    let sample = produced
                        .ok_or_else(|| anyhow!("the decoder announced a picture but gave none"))?;
                    Ok(Output::Frame(Box::new(self.frame_from(&sample)?)))
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Output::NeedMoreInput),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // The decoder read the stream's own headers and wants to
                    // hand over a different shape than we asked for.
                    self.output = configure_output(&self.transform)?;
                    self.spare_output = None;
                    info!(
                        "the stream changed shape: now {}x{}",
                        self.output.width, self.output.height
                    );
                    Ok(Output::FormatChanged)
                }
                Err(e) => Err(e).context("the decoder failed"),
            }
        }
    }

    fn frame_from(&self, sample: &IMFSample) -> Result<Nv12Frame> {
        let stride = self.output.stride;
        let visible_rows = self.output.height as usize;
        let wanted = Nv12Frame::buffer_len(stride, self.output.height);
        let mut data = vec![0u8; wanted];
        unsafe {
            let buffer = sample
                .ConvertToContiguousBuffer()
                .context("cannot read the decoded picture")?;
            let mut start: *mut u8 = ptr::null_mut();
            let mut length: u32 = 0;
            buffer
                .Lock(&mut start, None, Some(&mut length))
                .context("cannot lock the decoded picture")?;

            // Copy the two planes separately: in the decoder's buffer the
            // colour plane sits after the *padded* rows, in ours after the
            // visible ones.
            let available = length as usize;
            let brightness = (stride * visible_rows).min(available);
            ptr::copy_nonoverlapping(start, data.as_mut_ptr(), brightness);

            let colour_start = stride * self.output.coded_height as usize;
            let colour_len =
                (stride * visible_rows / 2).min(available.saturating_sub(colour_start));
            if colour_len > 0 {
                ptr::copy_nonoverlapping(
                    start.add(colour_start),
                    data.as_mut_ptr().add(stride * visible_rows),
                    colour_len,
                );
            }
            let _ = buffer.Unlock();

            let pts_hns = sample.GetSampleTime().unwrap_or(0).max(0);
            Ok(Nv12Frame {
                width: self.output.width,
                height: self.output.height,
                stride: self.output.stride,
                data,
                pts_us: (pts_hns / HNS_PER_MICROSECOND) as u64,
            })
        }
    }

    /// The buffer we lend the decoder, made once and reused.
    fn spare_sample(&mut self) -> Result<IMFSample> {
        if let Some(sample) = &self.spare_output {
            unsafe {
                let buffer = sample.GetBufferByIndex(0)?;
                buffer.SetCurrentLength(0)?;
            }
            return Ok(sample.clone());
        }
        unsafe {
            let sample = MFCreateSample()?;
            let buffer = MFCreateMemoryBuffer(self.output.buffer_len as u32)?;
            sample.AddBuffer(&buffer)?;
            self.spare_output = Some(sample.clone());
            Ok(sample)
        }
    }

    fn input_sample(&self, access_unit: &[u8], pts_us: u64) -> Result<IMFSample> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(access_unit.len() as u32)?;
            let mut start: *mut u8 = ptr::null_mut();
            buffer.Lock(&mut start, None, None)?;
            ptr::copy_nonoverlapping(access_unit.as_ptr(), start, access_unit.len());
            let _ = buffer.Unlock();
            buffer.SetCurrentLength(access_unit.len() as u32)?;

            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(pts_us as i64 * HNS_PER_MICROSECOND)?;
            Ok(sample)
        }
    }
}

impl Decoder for MediaFoundationDecoder {
    fn decode(&mut self, access_unit: &[u8], pts_us: u64) -> Result<Option<Nv12Frame>> {
        if access_unit.is_empty() {
            return Ok(None);
        }
        let sample = self.input_sample(access_unit, pts_us)?;
        let mut earlier = None;
        unsafe {
            if let Err(e) = self.transform.ProcessInput(0, &sample, 0) {
                if e.code() != MF_E_NOTACCEPTING {
                    return Err(e).context("the decoder refused a frame");
                }
                // It is full: take what is ready, then it has room again.
                earlier = self.drain()?;
                self.transform
                    .ProcessInput(0, &sample, 0)
                    .context("the decoder refused a frame after draining")?;
            }
        }
        Ok(self.drain()?.or(earlier).map(|frame| *frame))
    }

    fn flush(&mut self) -> Result<()> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0)
                .context("cannot reset the decoder")?;
        }
        Ok(())
    }
}

/// Picks NV12 out of what the decoder offers and reads back the exact shape.
fn configure_output(transform: &IMFTransform) -> Result<OutputFormat> {
    unsafe {
        let mut index = 0;
        loop {
            let Ok(candidate) = transform.GetOutputAvailableType(0, index) else {
                bail!("the decoder does not offer NV12 pictures");
            };
            if candidate.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                transform
                    .SetOutputType(0, &candidate, 0)
                    .context("the decoder rejected NV12")?;
                break;
            }
            index += 1;
        }

        let current = transform
            .GetOutputCurrentType(0)
            .context("cannot read back the decoder's output format")?;
        let size = current.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
        let (coded_width, coded_height) = unpack_size(size);
        // Decoders round the picture up to whole macroblocks — 828 becomes
        // 832 — and describe the part that is really the picture separately.
        // Without this the window would show a strip of padding.
        let (width, height) = visible_area(&current).unwrap_or((coded_width, coded_height));
        if (width, height) != (coded_width, coded_height) {
            debug!("the decoder pads {width}x{height} out to {coded_width}x{coded_height}");
        }
        let stride = match current.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
            // A negative stride means bottom-up, which NV12 never is here.
            Ok(stride) => (stride as i32).unsigned_abs() as usize,
            Err(_) => width as usize,
        }
        .max(width as usize);

        let info = transform
            .GetOutputStreamInfo(0)
            .context("cannot ask the decoder about its output")?;
        let provides =
            (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32;
        let decoder_allocates = info.dwFlags & provides != 0;
        let buffer_len = (info.cbSize as usize).max(Nv12Frame::buffer_len(stride, coded_height));

        Ok(OutputFormat {
            width,
            height,
            coded_height,
            stride,
            buffer_len,
            decoder_allocates,
        })
    }
}

/// The part of the decoder's output that is really the picture.
unsafe fn visible_area(media_type: &IMFMediaType) -> Option<(u32, u32)> {
    let mut area = MFVideoArea::default();
    let bytes = std::slice::from_raw_parts_mut(
        &mut area as *mut MFVideoArea as *mut u8,
        std::mem::size_of::<MFVideoArea>(),
    );
    media_type
        .GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None)
        .ok()?;
    let width = u32::try_from(area.Area.cx).ok()?;
    let height = u32::try_from(area.Area.cy).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    if area.OffsetX.value != 0 || area.OffsetY.value != 0 {
        debug!(
            "the picture starts at {},{} inside the decoder's buffer",
            area.OffsetX.value, area.OffsetY.value
        );
    }
    Some((width, height))
}

fn pack_size(width: u32, height: u32) -> u64 {
    ((width as u64) << 32) | height as u64
}

fn unpack_size(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_sizes_pack_the_way_media_foundation_wants() {
        assert_eq!(unpack_size(pack_size(828, 1792)), (828, 1792));
    }

    #[test]
    fn windows_can_decode_h264() {
        // Every supported Windows ships this decoder; if this fails, the
        // receiver would show nothing at all on this machine.
        assert!(is_supported(Codec::H264));
    }
}
