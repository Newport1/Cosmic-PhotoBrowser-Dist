pub const HISTOGRAM_BINS: usize = 64;

/// Luminance histogram (Rec.601-ish: 0.299R+0.587G+0.114B) over RGBA8 pixels, HISTOGRAM_BINS buckets.
/// Returns per-bin counts. Pure; unit-tested.
pub fn luminance_histogram(rgba: &[u8]) -> [u32; HISTOGRAM_BINS] {
    let mut bins = [0u32; HISTOGRAM_BINS];
    let n = rgba.len();
    let mut i = 0;
    while i + 3 < n {
        let r = rgba[i] as f32;
        let g = rgba[i + 1] as f32;
        let b = rgba[i + 2] as f32;
        // ignore alpha at i+3
        let lum = 0.299 * r + 0.587 * g + 0.114 * b;
        let lum_u8 = lum.clamp(0.0, 255.0) as u8;
        let mut idx = (lum_u8 as usize * HISTOGRAM_BINS) / 256;
        if idx >= HISTOGRAM_BINS {
            idx = HISTOGRAM_BINS - 1;
        }
        bins[idx] += 1;
        i += 4;
    }
    bins
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_black_in_bin_0() {
        let data = [0u8, 0, 0, 255, 0, 0, 0, 0]; // 2 pixels
        let h = luminance_histogram(&data);
        assert_eq!(h[0], 2);
        assert_eq!(h.iter().skip(1).sum::<u32>(), 0);
    }

    #[test]
    fn all_white_in_top_bin() {
        let data = [255u8, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255]; // 3 pixels
        let h = luminance_histogram(&data);
        assert_eq!(h[HISTOGRAM_BINS - 1], 3);
        assert_eq!(h.iter().take(HISTOGRAM_BINS - 1).sum::<u32>(), 0);
    }

    #[test]
    fn mid_grey_expected_bin() {
        // 128 lum -> (128 * 64) / 256 = 32 exactly
        let data = [128u8, 128, 128, 255];
        let h = luminance_histogram(&data);
        assert_eq!(h[32], 1);
        assert_eq!(h.iter().filter(|&&c| c != 0).count(), 1);
    }

    #[test]
    fn total_count_matches_pixels() {
        let data = vec![100u8; 4 * 10];
        let h: u32 = luminance_histogram(&data).iter().sum();
        assert_eq!(h, 10);
    }

    #[test]
    fn empty_does_not_panic() {
        let h = luminance_histogram(&[]);
        assert!(h.iter().all(|&c| c == 0));
    }

    #[test]
    fn non_multiple_4_tail_safe() {
        // 1 full pixel (white) + 1 full (black) + 2-byte tail
        let mut data = vec![0u8; 4 * 2 + 2];
        data[0] = 255;
        data[1] = 255;
        data[2] = 255;
        // second pixel black (index 4,5,6)
        let h = luminance_histogram(&data);
        assert_eq!(h[0], 1); // black
        assert_eq!(h[HISTOGRAM_BINS - 1], 1); // white
        assert_eq!(h.iter().sum::<u32>(), 2);
    }
}
