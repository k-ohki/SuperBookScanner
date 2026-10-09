//! 傾き補正。文字のインク画素を回転させて投影し、投影ヒストグラムが最も鋭くなる角度を探す。
//! 横書きは行方向 (水平)、縦書きは列方向 (垂直) の投影を両方試し、より鋭い方を採用する。

use crate::imgutil;
use image::{Rgb, RgbImage};
use imageproc::geometric_transformations::{rotate_about_center, Border, Interpolation};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DeskewOptions {
    /// 探索する最大角度 (度)
    pub max_angle_deg: f64,
    /// これより小さい傾きは補正しない (度)
    pub min_angle_deg: f64,
    /// 解析用縮小画像の長辺
    pub analysis_max_size: u32,
}

impl Default for DeskewOptions {
    fn default() -> Self {
        Self {
            max_angle_deg: 5.0,
            min_angle_deg: 0.05,
            analysis_max_size: 1200,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DeskewResult {
    /// 検出した傾き (度)。画像はこの角度だけ逆に回して補正する
    pub angle_deg: f64,
    pub applied: bool,
}

/// 傾きを検出する (度。正 = 時計回りに傾いている)。インクが少なすぎる場合は None。
pub fn detect_skew(img: &RgbImage, options: &DeskewOptions) -> Option<f64> {
    let gray = imgutil::to_gray(img);
    let (small, _) = imgutil::shrink_to_max_side(&gray, options.analysis_max_size);
    let (w, h) = (small.width() as usize, small.height() as usize);

    // 局所 2 値化と大津法の AND で、照明ムラやノドの影を拾わずに文字だけを取る
    let block = (((w.min(h) / 30) | 1) as u32).max(15);
    let mut ink = imgutil::adaptive_threshold_inv(&small, block, 10.0);
    let otsu = imgutil::otsu_inv(&small);
    for (a, b) in ink.as_mut().iter_mut().zip(otsu.as_raw()) {
        *a &= *b;
    }

    // 外周 3% は無視 (紙の端・机などが写り込むため)
    let (bx, by) = (w * 3 / 100, h * 3 / 100);
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for y in by..h - by {
        for x in bx..w - bx {
            if ink.as_raw()[y * w + x] != 0 {
                pts.push((x as f64 - w as f64 / 2.0, y as f64 - h as f64 / 2.0));
            }
        }
    }
    if pts.len() < 500 {
        return None;
    }

    let (cx, cy) = (w as f64, h as f64);
    let best = |vertical: bool| -> (f64, f64) {
        let score = |deg: f64| -> f64 {
            let t = deg.to_radians();
            let (s, c) = t.sin_cos();
            let size = (cx.hypot(cy) as usize) + 2;
            let mut hist = vec![0u32; size];
            let off = size as f64 / 2.0;
            for &(x, y) in &pts {
                // 点を -t だけ回したときの y (横書き) / x (縦書き) 座標
                let v = if vertical { x * c + y * s } else { y * c - x * s };
                let i = (v + off) as usize;
                if i < size {
                    hist[i] += 1;
                }
            }
            hist.iter().map(|&n| (n as f64) * (n as f64)).sum()
        };
        let mut best_deg = 0.0;
        let mut best_score = f64::MIN;
        let (mut sum, mut n) = (0.0, 0.0);
        let mut deg = -options.max_angle_deg;
        while deg <= options.max_angle_deg + 1e-9 {
            let sc = score(deg);
            sum += sc;
            n += 1.0;
            if sc > best_score {
                best_score = sc;
                best_deg = deg;
            }
            deg += 0.25;
        }
        let center = best_deg;
        let mut deg = center - 0.25;
        while deg <= center + 0.25 + 1e-9 {
            let sc = score(deg);
            if sc > best_score {
                best_score = sc;
                best_deg = deg;
            }
            deg += 0.02;
        }
        // 探索範囲の平均との比で「鋭さ」を正規化して、横と縦を比べられるようにする
        (best_deg, best_score / (sum / n).max(1.0))
    };

    let (h_deg, h_score) = best(false);
    let (v_deg, v_score) = best(true);
    Some(if v_score > h_score * 1.1 { v_deg } else { h_deg })
}

/// 傾きを検出して補正する。
pub fn deskew(img: &RgbImage, options: &DeskewOptions) -> (RgbImage, DeskewResult) {
    let Some(angle) = detect_skew(img, options) else {
        return (img.clone(), DeskewResult::default());
    };
    if angle.abs() < options.min_angle_deg {
        return (
            img.clone(),
            DeskewResult {
                angle_deg: angle,
                applied: false,
            },
        );
    }
    let out = rotate_about_center(
        img,
        -(angle.to_radians() as f32),
        Interpolation::Bicubic,
        Border::Constant(Rgb([255, 255, 255])),
    );
    (
        out,
        DeskewResult {
            angle_deg: angle,
            applied: true,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use imageproc::drawing::draw_filled_rect_mut;
    use imageproc::rect::Rect;

    fn text_like_page() -> RgbImage {
        let mut img = RgbImage::from_pixel(1000, 1400, Rgb([255, 255, 255]));
        for line in 0..30 {
            let y = 150 + line * 36;
            let mut x = 120;
            while x < 880 {
                draw_filled_rect_mut(&mut img, Rect::at(x, y).of_size(16, 20), Rgb([20, 20, 20]));
                x += 22;
            }
        }
        img
    }

    #[test]
    fn detects_and_corrects_rotation() {
        let page = text_like_page();
        for &deg in &[2.0f64, -1.5] {
            // 時計回りに deg 度傾けた画像を作る
            let tilted = rotate_about_center(&page, deg.to_radians() as f32, Interpolation::Bilinear, Border::Constant(Rgb([255, 255, 255])));
            let found = detect_skew(&tilted, &DeskewOptions::default()).unwrap();
            assert!((found - deg).abs() < 0.1, "expected {deg}, found {found}");
            let (fixed, r) = deskew(&tilted, &DeskewOptions::default());
            assert!(r.applied);
            let again = detect_skew(&fixed, &DeskewOptions::default()).unwrap();
            assert!(again.abs() < 0.1, "residual {again}");
        }
    }
}
