//! Decoded pictures and how they reach the screen.
//!
//! Hardware decoders hand back NV12: an 8-bit brightness plane followed by a
//! half-size plane of interleaved colour. Rather than convert a whole frame
//! and then scale it, the blit below samples the source once per pixel it
//! actually draws — the work is bounded by the window, not by the phone's
//! screen.

/// One decoded picture, exactly as the decoder produced it.
#[derive(Clone)]
pub struct Nv12Frame {
    pub width: u32,
    pub height: u32,
    /// Bytes per row, the same for both planes; usually more than `width`.
    pub stride: usize,
    /// The brightness plane, then the colour plane.
    pub data: Vec<u8>,
    /// The phone's timestamp for this picture.
    pub pts_us: u64,
}

impl Nv12Frame {
    /// How many bytes a frame of this shape needs.
    pub fn buffer_len(stride: usize, height: u32) -> usize {
        stride * height as usize * 3 / 2
    }

    fn y_plane(&self) -> &[u8] {
        let end = self.stride * self.height as usize;
        &self.data[..end.min(self.data.len())]
    }

    fn uv_plane(&self) -> &[u8] {
        let start = self.stride * self.height as usize;
        if start >= self.data.len() {
            &[]
        } else {
            &self.data[start..]
        }
    }

    /// Draws the picture into a `dst_width` x `dst_height` buffer of `0RGB`
    /// pixels, scaled to fit and centred, with `background` around it.
    pub fn blit_fit(&self, dst: &mut [u32], dst_width: u32, dst_height: u32, background: u32) {
        let (dw, dh) = (dst_width as usize, dst_height as usize);
        if dw == 0 || dh == 0 || dst.len() < dw * dh {
            return;
        }
        if self.width == 0 || self.height == 0 {
            dst[..dw * dh].fill(background);
            return;
        }

        let (sw, sh) = (self.width as usize, self.height as usize);
        // The largest rectangle with the picture's shape that still fits.
        let (rect_w, rect_h) = if dw * sh <= dh * sw {
            (dw, (dw * sh / sw).max(1))
        } else {
            ((dh * sw / sh).max(1), dh)
        };
        let x0 = (dw - rect_w.min(dw)) / 2;
        let y0 = (dh - rect_h.min(dh)) / 2;
        let rect_w = rect_w.min(dw);
        let rect_h = rect_h.min(dh);

        let y_plane = self.y_plane();
        let uv_plane = self.uv_plane();

        for row in 0..dh {
            let line = &mut dst[row * dw..row * dw + dw];
            if row < y0 || row >= y0 + rect_h {
                line.fill(background);
                continue;
            }
            line[..x0].fill(background);
            line[x0 + rect_w..].fill(background);

            let sy = (row - y0) * sh / rect_h;
            let y_row = &y_plane[(sy * self.stride).min(y_plane.len())..];
            let uv_row = &uv_plane[((sy / 2) * self.stride).min(uv_plane.len())..];

            for (x, pixel) in line[x0..x0 + rect_w].iter_mut().enumerate() {
                let sx = x * sw / rect_w;
                let luma = *y_row.get(sx).unwrap_or(&16);
                let chroma = sx & !1;
                let cb = *uv_row.get(chroma).unwrap_or(&128);
                let cr = *uv_row.get(chroma + 1).unwrap_or(&128);
                *pixel = to_rgb(luma, cb, cr);
            }
        }
    }
}

/// BT.709 limited range — what phone encoders produce.
fn to_rgb(y: u8, cb: u8, cr: u8) -> u32 {
    let c = (y as i32 - 16) * 298;
    let d = cb as i32 - 128;
    let e = cr as i32 - 128;
    let r = clamp8((c + 459 * e + 128) >> 8);
    let g = clamp8((c - 55 * d - 136 * e + 128) >> 8);
    let b = clamp8((c + 541 * d + 128) >> 8);
    (r << 16) | (g << 8) | b
}

fn clamp8(value: i32) -> u32 {
    value.clamp(0, 255) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of one colour, with padding after each row like a real decoder.
    fn solid(width: u32, height: u32, y: u8, cb: u8, cr: u8) -> Nv12Frame {
        let stride = width as usize + 16;
        let mut data = vec![0u8; Nv12Frame::buffer_len(stride, height)];
        let split = stride * height as usize;
        data[..split].fill(y);
        for pair in data[split..].chunks_exact_mut(2) {
            pair[0] = cb;
            pair[1] = cr;
        }
        Nv12Frame {
            width,
            height,
            stride,
            data,
            pts_us: 0,
        }
    }

    #[test]
    fn grey_stays_grey_and_white_stays_white() {
        assert_eq!(to_rgb(16, 128, 128), 0x000000);
        assert_eq!(to_rgb(235, 128, 128), 0xFFFFFF);
    }

    #[test]
    fn colours_land_where_they_should() {
        // Limited-range BT.709 red, green and blue.
        let (r, g, b) = (
            to_rgb(63, 102, 240),
            to_rgb(173, 42, 26),
            to_rgb(32, 240, 118),
        );
        assert!(r >> 16 > 0xC0, "red channel dominates: {r:06X}");
        assert!((g >> 8) & 0xFF > 0xC0, "green channel dominates: {g:06X}");
        assert!(b & 0xFF > 0xC0, "blue channel dominates: {b:06X}");
    }

    #[test]
    fn a_tall_picture_is_letterboxed_left_and_right() {
        // A phone screen (tall) in a wide window: bars on the sides.
        let frame = solid(100, 200, 235, 128, 128);
        let (dw, dh) = (400u32, 200u32);
        let mut dst = vec![0u32; (dw * dh) as usize];
        frame.blit_fit(&mut dst, dw, dh, 0x101010);

        let row = &dst[(dh as usize / 2) * dw as usize..][..dw as usize];
        assert_eq!(row[0], 0x101010, "left bar");
        assert_eq!(row[dw as usize - 1], 0x101010, "right bar");
        assert_eq!(row[dw as usize / 2], 0xFFFFFF, "picture in the middle");
        // 100x200 into 400x200 keeps its shape: a 100-wide picture centred.
        assert_eq!(row.iter().filter(|p| **p == 0xFFFFFF).count(), 100);
    }

    #[test]
    fn a_wide_window_of_the_same_shape_has_no_bars() {
        let frame = solid(50, 100, 235, 128, 128);
        let (dw, dh) = (100u32, 200u32);
        let mut dst = vec![0u32; (dw * dh) as usize];
        frame.blit_fit(&mut dst, dw, dh, 0x101010);
        assert!(
            dst.iter().all(|p| *p == 0xFFFFFF),
            "the picture should fill a window of its own shape"
        );
    }

    #[test]
    fn a_window_of_nothing_is_not_a_crash() {
        let frame = solid(8, 8, 128, 128, 128);
        let mut dst = vec![0u32; 0];
        frame.blit_fit(&mut dst, 0, 0, 0);
    }
}
