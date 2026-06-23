use crate::xmp::DevelopParams;
use image::RgbImage;

/// Apply a develop-look APPROXIMATION of the core crs: edits to a demosaiced image.
/// Ops (in order): exposure (linear), contrast (S-curve), tone curve (point), vibrance+saturation (HSL).
/// NOT an exact ACR match. Returns the input unchanged when no relevant param is set.
pub fn apply_develop(img: &RgbImage, p: &DevelopParams) -> RgbImage {
    if !has_any(p) {
        return img.clone();
    }
    let curve = NormCurve::from_points(&p.tone_curve); // None if empty/identity
    let exposure = p.exposure.filter(|e| *e != 0.0);
    let hl = p
        .highlights
        .filter(|x| *x != 0)
        .map_or(0.0, |x| x as f32 / 100.0);
    let sh = p
        .shadows
        .filter(|x| *x != 0)
        .map_or(0.0, |x| x as f32 / 100.0);
    let wh = p
        .whites
        .filter(|x| *x != 0)
        .map_or(0.0, |x| x as f32 / 100.0);
    let bl = p
        .blacks
        .filter(|x| *x != 0)
        .map_or(0.0, |x| x as f32 / 100.0);
    let contrast = p.contrast.filter(|c| *c != 0).map(|c| c as f32 / 100.0);
    let sat = p.saturation.filter(|s| *s != 0).map(|s| s as f32 / 100.0);
    let vib = p.vibrance.filter(|v| *v != 0).map(|v| v as f32 / 100.0);

    let tonal_active = hl != 0.0 || sh != 0.0 || wh != 0.0 || bl != 0.0;

    let mut out = img.clone();
    for px in out.pixels_mut() {
        let mut rgb = [
            px[0] as f32 / 255.0,
            px[1] as f32 / 255.0,
            px[2] as f32 / 255.0,
        ];
        if let Some(ev) = exposure {
            rgb = apply_exposure(rgb, ev);
        }
        if tonal_active {
            for v in &mut rgb {
                *v = apply_tone_regions(*v, hl, sh, wh, bl);
            }
        }
        if let Some(c) = contrast {
            for v in &mut rgb {
                *v = apply_contrast(*v, c);
            }
        }
        if let Some(curve) = &curve {
            for v in &mut rgb {
                *v = curve.eval(*v);
            }
        }
        if sat.is_some() || vib.is_some() {
            rgb = apply_sat_vib(rgb, sat.unwrap_or(0.0), vib.unwrap_or(0.0));
        }
        px[0] = to_u8(rgb[0]);
        px[1] = to_u8(rgb[1]);
        px[2] = to_u8(rgb[2]);
    }
    out
}

fn has_any(p: &DevelopParams) -> bool {
    p.exposure.is_some_and(|e| e != 0.0)
        || p.contrast.is_some_and(|c| c != 0)
        || p.saturation.is_some_and(|s| s != 0)
        || p.vibrance.is_some_and(|v| v != 0)
        || !p.tone_curve.is_empty()
        || p.highlights.is_some_and(|h| h != 0)
        || p.shadows.is_some_and(|s| s != 0)
        || p.whites.is_some_and(|w| w != 0)
        || p.blacks.is_some_and(|b| b != 0)
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn apply_exposure(rgb: [f32; 3], ev: f32) -> [f32; 3] {
    let f = 2f32.powf(ev);
    let lin: palette::LinSrgb = palette::Srgb::new(rgb[0], rgb[1], rgb[2]).into_linear();
    let scaled = palette::LinSrgb::new(
        (lin.red * f).clamp(0.0, 1.0),
        (lin.green * f).clamp(0.0, 1.0),
        (lin.blue * f).clamp(0.0, 1.0),
    );
    let s: palette::Srgb = palette::Srgb::from_linear(scaled);
    [s.red, s.green, s.blue]
}

fn apply_contrast(v: f32, c: f32) -> f32 {
    (0.5 + (v - 0.5) * (1.0 + c)).clamp(0.0, 1.0)
}

/// Apply highlights/shadows/whites/blacks to a single channel value (display [0,1]).
/// Region-weighted approximation: each slider's effect concentrates on its tonal region.
/// Sliders are the raw -100..100 ints; n = slider/100 ∈ [-1,1]. Returns clamped [0,1].
fn apply_tone_regions(v: f32, highlights: f32, shadows: f32, whites: f32, blacks: f32) -> f32 {
    let mut v = v;
    // blacks: darkest tones (very dark-weighted); + lifts, - deepens
    v += blacks * 0.25 * (1.0 - v).powi(3);
    // shadows: broad dark region; + lifts, - lowers
    v += shadows * 0.25 * (1.0 - v).powi(2);
    // highlights: bright region; + boosts, - recovers
    v += highlights * 0.25 * v.powi(2);
    // whites: brightest tones (very bright-weighted)
    v += whites * 0.25 * v.powi(3);
    v.clamp(0.0, 1.0)
}

#[derive(Clone, Debug, PartialEq)]
struct NormCurve {
    points: Vec<(f32, f32)>, // normalized [0,1], sorted ascending x, deduped
}

impl NormCurve {
    /// Normalize (x/255,y/255), sort by x, dedup same-x (later wins), return None for empty or exact identity [(0,0),(1,1)]
    fn from_points(pts: &[(f32, f32)]) -> Option<Self> {
        if pts.is_empty() {
            return None;
        }
        let mut norm: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| (x / 255.0, y / 255.0)).collect();
        norm.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        // Dedup: keep later y for duplicate x
        let mut deduped: Vec<(f32, f32)> = Vec::new();
        for p in norm {
            if let Some(&(lx, _)) = deduped.last() {
                if (lx - p.0).abs() < 1e-6 {
                    *deduped.last_mut().unwrap() = p;
                    continue;
                }
            }
            deduped.push(p);
        }

        if deduped.is_empty() {
            return None;
        }
        let is_exact_identity = deduped.len() == 2
            && (deduped[0].0 - 0.0).abs() < 1e-6
            && (deduped[0].1 - 0.0).abs() < 1e-6
            && (deduped[1].0 - 1.0).abs() < 1e-6
            && (deduped[1].1 - 1.0).abs() < 1e-6;
        if is_exact_identity {
            return None;
        }
        Some(Self { points: deduped })
    }

    /// Clamp input v to curve's x-range [x0, xN], then piecewise linear interp; endpoints constant outside.
    fn eval(&self, v: f32) -> f32 {
        let pts = &self.points;
        if pts.is_empty() {
            return v.clamp(0.0, 1.0);
        }
        let x0 = pts[0].0;
        let xn = pts.last().unwrap().0;
        let v = v.clamp(x0, xn);
        if v <= x0 {
            return pts[0].1;
        }
        if v >= xn {
            return pts.last().unwrap().1;
        }
        // Find containing segment (guaranteed to exist)
        for i in 0..pts.len() - 1 {
            let (x0i, y0i) = pts[i];
            let (x1i, y1i) = pts[i + 1];
            if v >= x0i && v <= x1i {
                let dx = x1i - x0i;
                if dx.abs() < 1e-9 {
                    return y0i;
                }
                let t = (v - x0i) / dx;
                return y0i + t * (y1i - y0i);
            }
        }
        pts.last().unwrap().1
    }
}

fn apply_sat_vib(rgb: [f32; 3], sat: f32, vib: f32) -> [f32; 3] {
    use palette::IntoColor;
    let hsl: palette::Hsl = palette::Srgb::new(rgb[0], rgb[1], rgb[2]).into_color();
    let s0 = hsl.saturation;
    // saturation: uniform scale; vibrance: stronger where current saturation is LOW (protects already-saturated)
    let mut s = s0 * (1.0 + sat) + vib * (1.0 - s0) * s0;
    s = s.clamp(0.0, 1.0);
    let hsl2 = palette::Hsl::new(hsl.hue, s, hsl.lightness);
    let srgb: palette::Srgb = hsl2.into_color();
    [
        srgb.red.clamp(0.0, 1.0),
        srgb.green.clamp(0.0, 1.0),
        srgb.blue.clamp(0.0, 1.0),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbImage;

    fn make_img(pixels: &[(u8, u8, u8)]) -> RgbImage {
        let mut img = RgbImage::new(pixels.len() as u32, 1);
        for (i, &(r, g, b)) in pixels.iter().enumerate() {
            img.put_pixel(i as u32, 0, image::Rgb([r, g, b]));
        }
        img
    }

    fn pixels(img: &RgbImage) -> Vec<(u8, u8, u8)> {
        img.pixels().map(|p| (p[0], p[1], p[2])).collect()
    }

    #[test]
    fn no_params_returns_unchanged() {
        let p = DevelopParams::default();
        let img = make_img(&[(10, 20, 30), (128, 128, 128), (200, 50, 50)]);
        let out = apply_develop(&img, &p);
        assert_eq!(pixels(&out), pixels(&img));
    }

    #[test]
    fn identity_tone_curve_unchanged() {
        let p = DevelopParams {
            tone_curve: vec![(0.0, 0.0), (255.0, 255.0)],
            ..DevelopParams::default()
        };
        let img = make_img(&[(0, 0, 0), (64, 64, 64), (128, 128, 128), (255, 255, 255)]);
        let out = apply_develop(&img, &p);
        assert_eq!(pixels(&out), pixels(&img));
    }

    #[test]
    fn exposure_plus_one_brightens_midgray() {
        let img = make_img(&[(128, 128, 128)]);
        let p = DevelopParams {
            exposure: Some(1.0),
            ..DevelopParams::default()
        };
        let out = apply_develop(&img, &p);
        assert!(
            out.get_pixel(0, 0)[0] > 128,
            "exposure +1 should brighten 128"
        );
        let p = DevelopParams {
            exposure: Some(-1.0),
            ..DevelopParams::default()
        };
        let out = apply_develop(&img, &p);
        assert!(
            out.get_pixel(0, 0)[0] < 128,
            "exposure -1 should darken 128"
        );
    }

    #[test]
    fn contrast_positive_pushes_from_midpoint() {
        let img = make_img(&[(50, 50, 50), (200, 200, 200)]);
        let p = DevelopParams {
            contrast: Some(50), // c=0.5
            ..DevelopParams::default()
        };
        let out = apply_develop(&img, &p);
        // light should get lighter, dark darker
        assert!(
            out.get_pixel(0, 0)[0] < 50,
            "dark pixel should get darker with +contrast"
        );
        assert!(
            out.get_pixel(1, 0)[0] > 200,
            "light pixel should get lighter with +contrast"
        );
        // zero unchanged
        let p0 = DevelopParams {
            contrast: Some(0),
            ..DevelopParams::default()
        };
        let out0 = apply_develop(&img, &p0);
        assert_eq!(pixels(&out0), pixels(&img));
    }

    #[test]
    fn saturation_minus_100_is_grayscale() {
        let img = make_img(&[(200, 50, 50)]);
        let p = DevelopParams {
            saturation: Some(-100),
            ..DevelopParams::default()
        };
        let out = apply_develop(&img, &p);
        let (r, g, b) = {
            let px = out.get_pixel(0, 0);
            (px[0], px[1], px[2])
        };
        // near gray within tolerance
        assert!((r as i32 - g as i32).abs() <= 2);
        assert!((g as i32 - b as i32).abs() <= 2);
        assert!((r as i32 - b as i32).abs() <= 2);
    }

    #[test]
    fn saturation_plus_increases_chroma() {
        let img = make_img(&[(180, 120, 120)]);
        let input = pixels(&img)[0];
        let p = DevelopParams {
            saturation: Some(80),
            ..DevelopParams::default()
        };
        let out = apply_develop(&img, &p);
        let outp = pixels(&out)[0];
        let spread_in = (input.0 as i32 - input.1 as i32).abs(); // rough chroma proxy: max-min
        let spread_out = (outp.0 as i32 - outp.1 as i32).abs();
        assert!(
            spread_out > spread_in,
            "saturation increase should increase channel spread"
        );
    }

    #[test]
    fn tone_curve_lifts_midtones() {
        let img = make_img(&[(128, 128, 128)]);
        let p = DevelopParams {
            tone_curve: vec![(0.0, 0.0), (128.0, 160.0), (255.0, 255.0)],
            ..DevelopParams::default()
        };
        let out = apply_develop(&img, &p);
        let val = out.get_pixel(0, 0)[0];
        // ~160 within tolerance (rounding etc)
        assert!(
            (val as i32 - 160).abs() <= 3,
            "mid 128 should map near 160, got {}",
            val
        );
    }

    #[test]
    fn clamps_no_panic_on_extremes() {
        let img = make_img(&[(255, 255, 255); 4]);
        let p = DevelopParams {
            exposure: Some(5.0),
            ..DevelopParams::default()
        };
        let out = apply_develop(&img, &p);
        // must not panic, and clamps to 255
        for px in out.pixels() {
            assert_eq!(px[0], 255);
            assert_eq!(px[1], 255);
            assert_eq!(px[2], 255);
        }
    }

    #[test]
    fn blacks_positive_lifts_darks_not_brights() {
        let img = make_img(&[(20, 20, 20), (230, 230, 230)]);
        let p = DevelopParams {
            blacks: Some(80),
            ..DevelopParams::default()
        };
        let out = apply_develop(&img, &p);
        let dark = out.get_pixel(0, 0)[0];
        let bright = out.get_pixel(1, 0)[0];
        assert!(dark > 20, "blacks+ should lift darks");
        assert!(
            (bright as i32 - 230).abs() <= 2,
            "blacks+ should leave brights ~unchanged"
        );
        // negative blacks darkens darks
        let p_neg = DevelopParams {
            blacks: Some(-80),
            ..DevelopParams::default()
        };
        let out_neg = apply_develop(&img, &p_neg);
        let dark_neg = out_neg.get_pixel(0, 0)[0];
        assert!(dark_neg < 20, "blacks- should deepen darks");
    }

    #[test]
    fn whites_positive_brightens_brights_not_darks() {
        let img = make_img(&[(20, 20, 20), (230, 230, 230)]);
        let p = DevelopParams {
            whites: Some(80),
            ..DevelopParams::default()
        };
        let out = apply_develop(&img, &p);
        let dark = out.get_pixel(0, 0)[0];
        let bright = out.get_pixel(1, 0)[0];
        assert!(bright > 230, "whites+ should brighten brights");
        assert!(
            (dark as i32 - 20).abs() <= 2,
            "whites+ should leave darks ~unchanged"
        );
    }

    #[test]
    fn highlights_negative_recovers_brights() {
        let img = make_img(&[(20, 20, 20), (240, 240, 240)]);
        let p = DevelopParams {
            highlights: Some(-80),
            ..DevelopParams::default()
        };
        let out = apply_develop(&img, &p);
        let dark = out.get_pixel(0, 0)[0];
        let bright = out.get_pixel(1, 0)[0];
        assert!(bright < 240, "highlights- should recover brights");
        assert!(
            (dark as i32 - 20).abs() <= 2,
            "highlights- should leave darks ~unchanged"
        );
    }

    #[test]
    fn shadows_positive_lifts_dark_region() {
        let img = make_img(&[(60, 60, 60)]);
        let p = DevelopParams {
            shadows: Some(80),
            ..DevelopParams::default()
        };
        let out = apply_develop(&img, &p);
        let val = out.get_pixel(0, 0)[0];
        assert!(val > 60, "shadows+ should lift mid-dark region");
    }

    #[test]
    fn tonal_none_is_unchanged() {
        let img = make_img(&[(10, 20, 30), (60, 60, 60), (200, 200, 200), (240, 240, 240)]);
        let p = DevelopParams::default();
        let out = apply_develop(&img, &p);
        assert_eq!(pixels(&out), pixels(&img));
    }
}
