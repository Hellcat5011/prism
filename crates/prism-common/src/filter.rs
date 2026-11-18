//! Colour filters for wallpapers, chosen in the wallpaper selector. Tone filters are a small
//! grade (saturation, contrast, lift); the others remap each pixel. Colour Pop
//! first finds the picture's dominant hue from a sample of its pixels.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Filter {
    #[default]
    Normal,
    ColorPop,
    PopArt,
    Synthwave,
    Noir,
    Vivid,
}

/// A filter ready to apply to the pixels of one picture.
#[derive(Clone, Copy, Debug)]
pub enum Grade {
    Tone { saturation: f32, contrast: f32, lift: f32 },
    /// Keeps colours near this hue (degrees), the rest turns grey.
    ColorPop(Option<f32>),
    PopArt,
    Synthwave,
}

impl Filter {
    /// In the order the selector shows them.
    pub const ALL: [Filter; 6] = [Filter::Normal, Filter::ColorPop, Filter::PopArt, Filter::Synthwave, Filter::Noir, Filter::Vivid];

    pub fn label(self) -> &'static str {
        match self {
            Filter::Normal => "Normal",
            Filter::ColorPop => "Color Pop",
            Filter::PopArt => "Pop Art",
            Filter::Synthwave => "Synthwave",
            Filter::Noir => "Noir",
            Filter::Vivid => "Vivid",
        }
    }

    /// Name stored on disk for the filter in use.
    pub fn id(self) -> &'static str {
        match self {
            Filter::Normal => "normal",
            Filter::ColorPop => "color-pop",
            Filter::PopArt => "pop-art",
            Filter::Synthwave => "synthwave",
            Filter::Noir => "noir",
            Filter::Vivid => "vivid",
        }
    }

    pub fn from_id(id: &str) -> Filter {
        Filter::ALL.into_iter().find(|f| f.id() == id).unwrap_or_default()
    }

    /// No colour left to build a theme from: matugen reads the unfiltered picture instead.
    pub fn is_monochrome(self) -> bool {
        self == Filter::Noir
    }

    /// The grade for a picture; `samples` (some of its pixels) is only read by Colour Pop.
    pub fn grade(self, samples: impl Iterator<Item = [u8; 3]>) -> Grade {
        let tone = |saturation, contrast, lift| Grade::Tone { saturation, contrast, lift };
        match self {
            Filter::Normal => tone(1.0, 1.0, 0.0),
            Filter::ColorPop => Grade::ColorPop(dominant_hue(samples)),
            Filter::PopArt => Grade::PopArt,
            Filter::Synthwave => Grade::Synthwave,
            Filter::Noir => tone(0.0, 1.45, -0.04),
            Filter::Vivid => tone(1.45, 1.12, 0.0),
        }
    }
}

fn luma(c: [f32; 3]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// Hue in degrees and HSV saturation and value.
fn hsv(c: [f32; 3]) -> (f32, f32, f32) {
    let max = c[0].max(c[1]).max(c[2]);
    let delta = max - c[0].min(c[1]).min(c[2]);
    if delta <= f32::EPSILON {
        return (0.0, 0.0, max);
    }
    let h = if max == c[0] {
        (c[1] - c[2]) / delta
    } else if max == c[1] {
        (c[2] - c[0]) / delta + 2.0
    } else {
        (c[0] - c[1]) / delta + 4.0
    };
    ((h * 60.0).rem_euclid(360.0), delta / max, max)
}

/// 0 at `from`, 1 at `to`, eased between (either direction).
fn smooth(from: f32, to: f32, v: f32) -> f32 {
    let t = ((v - from) / (to - from)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The hue with the most vivid colour; None for a picture without colour.
fn dominant_hue(samples: impl Iterator<Item = [u8; 3]>) -> Option<f32> {
    const BINS: usize = 36;
    let mut hist = [0f32; BINS];
    for s in samples {
        let (h, sat, val) = hsv(s.map(|v| v as f32 / 255.0));
        if sat > 0.2 && val > 0.15 {
            hist[(h / 10.0) as usize % BINS] += sat * val;
        }
    }
    let score = |i: usize| hist[(i + BINS - 1) % BINS] + 2.0 * hist[i] + hist[(i + 1) % BINS];
    let best = (0..BINS).max_by(|a, b| score(*a).total_cmp(&score(*b)))?;
    (score(best) > 0.0).then_some(best as f32 * 10.0 + 5.0)
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

impl Grade {
    pub fn apply(&self, rgb: [u8; 3]) -> [u8; 3] {
        let c = rgb.map(|v| v as f32 / 255.0);
        let l = luma(c);
        let out = match *self {
            Grade::Tone { saturation, contrast, lift } => c.map(|v| (l + saturation * (v - l) - 0.5) * contrast + 0.5 + lift),
            Grade::ColorPop(hue) => {
                let (h, s, _) = hsv(c);
                let keep = hue.map_or(0.0, |key| {
                    let d = (h - key).abs();
                    smooth(52.0, 28.0, d.min(360.0 - d)) * smooth(0.10, 0.26, s)
                });
                c.map(|v| l + (v - l) * keep * 1.3)
            }
            Grade::PopArt => c.map(|v| {
                let v = ((l + 1.6 * (v - l)) - 0.5) * 1.15 + 0.5;
                (v.clamp(0.0, 1.0) * 4.0).round() / 4.0
            }),
            Grade::Synthwave => {
                const SHADOW: [f32; 3] = [0.09, 0.05, 0.30];
                const MID: [f32; 3] = [0.88, 0.20, 0.56];
                const LIGHT: [f32; 3] = [1.0, 0.86, 0.56];
                let t = smooth(0.0, 1.0, l);
                if t < 0.5 { mix(SHADOW, MID, t * 2.0) } else { mix(MID, LIGHT, t * 2.0 - 1.0) }
            }
        };
        out.map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: [u8; 3] = [200, 90, 30];

    fn grade(f: Filter) -> Grade {
        f.grade(std::iter::repeat_n(SAMPLE, 32))
    }

    #[test]
    fn normal_keeps_every_colour() {
        let g = grade(Filter::Normal);
        for v in (0..=255u8).step_by(5) {
            assert_eq!(g.apply([v, 255 - v, v / 2]), [v, 255 - v, v / 2]);
        }
    }

    #[test]
    fn monochrome_filters_have_equal_channels() {
        for f in Filter::ALL.into_iter().filter(|f| f.is_monochrome()) {
            let [r, g, b] = grade(f).apply(SAMPLE);
            assert!(r == g && g == b, "{f:?}");
        }
    }

    #[test]
    fn every_other_filter_changes_the_picture() {
        for f in Filter::ALL.into_iter().filter(|f| *f != Filter::Normal) {
            assert_ne!(grade(f).apply(SAMPLE), SAMPLE, "{f:?}");
        }
    }

    #[test]
    fn color_pop_keeps_only_the_dominant_hue() {
        let g = Filter::ColorPop.grade(std::iter::repeat_n([220, 30, 30], 90).chain(std::iter::repeat_n([30, 30, 220], 10)));
        let [r, _, b] = g.apply([220, 30, 30]);
        assert!(r > b + 100);
        let [r, g2, b] = g.apply([30, 30, 220]);
        assert!(r == g2 && g2 == b);
    }

    #[test]
    fn color_pop_of_a_grey_picture_is_grey() {
        let g = Filter::ColorPop.grade(std::iter::repeat_n([90, 90, 90], 32));
        let [r, g2, b] = g.apply(SAMPLE);
        assert!(r == g2 && g2 == b);
    }

    #[test]
    fn ids_round_trip() {
        for f in Filter::ALL {
            assert_eq!(Filter::from_id(f.id()), f);
        }
    }
}
