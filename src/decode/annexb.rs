//! Annex-B: the shape the phone sends its video in.
//!
//! An access unit is a run of NAL units separated by start codes. Windows
//! takes that as it comes; VideoToolbox wants the parameter sets handed over
//! separately and everything else prefixed with its length. The splitting
//! therefore lives here rather than in the macOS decoder — so it is tested
//! everywhere, not only where it happens to be used.

use crate::net::Codec;

/// The NAL units in a buffer, without their start codes and without the
/// trailing zeroes some encoders leave behind.
pub fn nal_units(data: &[u8]) -> NalUnits<'_> {
    NalUnits { data, at: 0 }
}

pub struct NalUnits<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Iterator for NalUnits<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        let start = start_code(self.data, self.at)?;
        let from = start.1;
        let end = match start_code(self.data, from) {
            Some((next_start, _)) => next_start,
            None => self.data.len(),
        };
        self.at = end;
        let unit = trim_trailing_zeroes(&self.data[from..end]);
        if unit.is_empty() {
            self.next()
        } else {
            Some(unit)
        }
    }
}

/// The next `00 00 01`, as (where it begins, where the payload begins).
fn start_code(data: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut at = from;
    while at + 3 <= data.len() {
        if data[at] == 0 && data[at + 1] == 0 && data[at + 2] == 1 {
            // A four-byte start code is the three-byte one with a zero in front.
            let begins = if at > from && data[at - 1] == 0 {
                at - 1
            } else {
                at
            };
            return Some((begins, at + 3));
        }
        at += 1;
    }
    None
}

fn trim_trailing_zeroes(unit: &[u8]) -> &[u8] {
    let mut end = unit.len();
    while end > 0 && unit[end - 1] == 0 {
        end -= 1;
    }
    &unit[..end]
}

/// The kind of NAL, in the numbering of its codec.
pub fn kind(unit: &[u8], codec: Codec) -> u8 {
    let Some(first) = unit.first() else {
        return 0;
    };
    match codec {
        Codec::H264 => first & 0x1F,
        Codec::Hevc => (first >> 1) & 0x3F,
    }
}

/// Whether this NAL is a parameter set — a description of the stream rather
/// than a piece of picture.
pub fn is_parameter_set(unit: &[u8], codec: Codec) -> bool {
    match codec {
        Codec::H264 => matches!(kind(unit, codec), 7 | 8),
        Codec::Hevc => matches!(kind(unit, codec), 32..=34),
    }
}

/// The parameter sets a stream needs before any picture can be decoded.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ParameterSets {
    /// HEVC only.
    pub vps: Option<Vec<u8>>,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

impl ParameterSets {
    /// Takes whatever this access unit carries. Returns whether anything
    /// changed — a change means the decoder has to be rebuilt.
    pub fn take_from(&mut self, access_unit: &[u8], codec: Codec) -> bool {
        let mut changed = false;
        for unit in nal_units(access_unit) {
            let slot = match (codec, kind(unit, codec)) {
                (Codec::H264, 7) | (Codec::Hevc, 33) => &mut self.sps,
                (Codec::H264, 8) | (Codec::Hevc, 34) => &mut self.pps,
                (Codec::Hevc, 32) => &mut self.vps,
                _ => continue,
            };
            if slot.as_deref() != Some(unit) {
                *slot = Some(unit.to_vec());
                changed = true;
            }
        }
        changed
    }

    /// Whether there is enough here to describe the stream.
    pub fn complete(&self, codec: Codec) -> bool {
        let base = self.sps.is_some() && self.pps.is_some();
        match codec {
            Codec::H264 => base,
            Codec::Hevc => base && self.vps.is_some(),
        }
    }

    /// The sets in the order VideoToolbox expects them.
    pub fn ordered(&self) -> Vec<&[u8]> {
        [
            self.vps.as_deref(),
            self.sps.as_deref(),
            self.pps.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// Rewrites an access unit with four-byte lengths in front of each NAL instead
/// of start codes, leaving the parameter sets out — they travel separately.
pub fn length_prefixed(access_unit: &[u8], codec: Codec, out: &mut Vec<u8>) {
    out.clear();
    for unit in nal_units(access_unit) {
        if is_parameter_set(unit, codec) {
            continue;
        }
        out.extend_from_slice(&(unit.len() as u32).to_be_bytes());
        out.extend_from_slice(unit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an Annex-B buffer, four-byte start codes as the phone sends them.
    fn annexb(units: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in units {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(unit);
        }
        out
    }

    #[test]
    fn units_come_back_out_the_way_they_went_in() {
        let stream = annexb(&[&[0x67, 1, 2], &[0x68, 3], &[0x65, 4, 5, 6]]);
        let units: Vec<&[u8]> = nal_units(&stream).collect();
        assert_eq!(
            units,
            vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5, 6][..]]
        );
    }

    #[test]
    fn three_byte_start_codes_are_understood_too() {
        let stream = [0, 0, 1, 0x67, 9, 0, 0, 1, 0x65, 8];
        let units: Vec<&[u8]> = nal_units(&stream).collect();
        assert_eq!(units, vec![&[0x67, 9][..], &[0x65, 8][..]]);
    }

    #[test]
    fn trailing_zeroes_are_not_part_of_a_unit() {
        let stream = [0, 0, 0, 1, 0x65, 7, 0, 0];
        let units: Vec<&[u8]> = nal_units(&stream).collect();
        assert_eq!(units, vec![&[0x65, 7][..]]);
    }

    #[test]
    fn rubbish_yields_nothing_rather_than_panicking() {
        assert_eq!(nal_units(&[]).count(), 0);
        assert_eq!(nal_units(&[0, 0, 0, 1]).count(), 0);
        assert_eq!(nal_units(&[1, 2, 3]).count(), 0);
    }

    #[test]
    fn h264_parameter_sets_are_recognised() {
        let stream = annexb(&[&[0x67, 0x42], &[0x68, 0xCE], &[0x65, 0x88]]);
        let mut sets = ParameterSets::default();
        assert!(sets.take_from(&stream, Codec::H264));
        assert!(sets.complete(Codec::H264));
        assert_eq!(sets.sps.as_deref(), Some(&[0x67, 0x42][..]));
        assert_eq!(sets.pps.as_deref(), Some(&[0x68, 0xCE][..]));
        assert_eq!(sets.ordered().len(), 2, "no VPS in H.264");
        // The same sets again change nothing, so no decoder is rebuilt.
        assert!(!sets.take_from(&stream, Codec::H264));
    }

    #[test]
    fn hevc_wants_a_vps_as_well() {
        // 32 = VPS, 33 = SPS, 34 = PPS, in the high bits of the first byte.
        let stream = annexb(&[&[32 << 1, 1], &[33 << 1, 2], &[34 << 1, 3], &[(19 << 1), 4]]);
        let mut sets = ParameterSets::default();
        assert!(sets.take_from(&stream, Codec::Hevc));
        assert!(sets.complete(Codec::Hevc));
        assert_eq!(sets.ordered().len(), 3);

        let mut without_vps = ParameterSets {
            vps: None,
            ..sets.clone()
        };
        assert!(!without_vps.complete(Codec::Hevc));
        assert!(without_vps.take_from(&stream, Codec::Hevc));
    }

    #[test]
    fn a_new_sps_is_noticed() {
        let first = annexb(&[&[0x67, 0x42], &[0x68, 0xCE]]);
        let second = annexb(&[&[0x67, 0x4D], &[0x68, 0xCE]]);
        let mut sets = ParameterSets::default();
        assert!(sets.take_from(&first, Codec::H264));
        assert!(
            sets.take_from(&second, Codec::H264),
            "a different SPS must be noticed"
        );
        assert_eq!(sets.sps.as_deref(), Some(&[0x67, 0x4D][..]));
    }

    #[test]
    fn length_prefixes_replace_start_codes_and_parameter_sets_are_left_out() {
        let stream = annexb(&[&[0x67, 1], &[0x68, 2], &[0x65, 3, 4, 5]]);
        let mut out = Vec::new();
        length_prefixed(&stream, Codec::H264, &mut out);
        assert_eq!(out, vec![0, 0, 0, 4, 0x65, 3, 4, 5]);
    }

    #[test]
    fn a_frame_of_several_slices_keeps_all_of_them() {
        let stream = annexb(&[&[0x65, 1], &[0x65, 2]]);
        let mut out = Vec::new();
        length_prefixed(&stream, Codec::H264, &mut out);
        assert_eq!(out, vec![0, 0, 0, 2, 0x65, 1, 0, 0, 0, 2, 0x65, 2]);
    }
}
