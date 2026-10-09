//! 見開き分割。横長の画像 (見開き) を、ノド (綴じ目) の位置で左右 2 ページに分ける。
//! (ScanTailor の Split Pages に相当する処理)
//!
//! ノドの位置は、画像中央付近 (幅の 35%〜65%) の列のうち、次の 2 つを合わせた点数が最も高い列とする。
//! - その列の近くに文字 (インク) がない (ページ間の余白)
//! - その列が周りより暗い (ノドの影・折り目)

use crate::imgutil;
use image::{imageops, RgbImage};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SplitOptions {
    /// 分割する: Auto = 横長 (幅 > 高さ × min_aspect) なら分割、Always = 常に分割、Never = 分割しない
    pub mode: SplitMode,
    pub min_aspect: f64,
    /// ノドを探す範囲 (画像幅に対する割合、中央からの片側)
    pub search_half_width: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitMode {
    Auto,
    Always,
    Never,
}

impl Default for SplitOptions {
    fn default() -> Self {
        Self {
            mode: SplitMode::Auto,
            min_aspect: 1.15,
            search_half_width: 0.15,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SplitResult {
    /// 分割した位置 (元画像の x)。分割しなかった場合は None
    pub gutter_x: Option<u32>,
}

/// 分割すべきなら、ノドの x 座標を返す。
pub fn find_gutter(img: &RgbImage, options: &SplitOptions) -> Option<u32> {
    let (w, h) = img.dimensions();
    match options.mode {
        SplitMode::Never => return None,
        SplitMode::Auto if (w as f64) < h as f64 * options.min_aspect => return None,
        _ => {}
    }

    let gray = imgutil::to_gray(img);
    let (small, scale) = imgutil::shrink_to_max_side(&gray, 1200);
    let (sw, sh) = (small.width() as usize, small.height() as usize);

    // インク (局所的に暗い画素)
    let block = (((sw.min(sh) / 30) | 1) as u32).max(15);
    let ink = imgutil::adaptive_threshold_inv(&small, block, 15.0);

    // 上下 10% は除く (本の外の写り込みが多いため)
    let (y0, y1) = (sh / 10, sh - sh / 10);
    let mut ink_col = vec![0f64; sw];
    let mut lum_col = vec![0f64; sw];
    for y in y0..y1 {
        for x in 0..sw {
            ink_col[x] += (ink.as_raw()[y * sw + x] != 0) as u32 as f64;
            lum_col[x] += small.as_raw()[y * sw + x] as f64;
        }
    }
    let rows = (y1 - y0) as f64;
    for x in 0..sw {
        ink_col[x] /= rows;
        lum_col[x] /= rows;
    }

    // 文字の列の間隔程度でならす
    let r = (sw / 100).max(2);
    let ink_s = box_smooth(&ink_col, r);
    let lum_s = box_smooth(&lum_col, r);
    let lum_wide = box_smooth(&lum_col, sw / 15);

    let lo = ((0.5 - options.search_half_width) * sw as f64) as usize;
    let hi = ((0.5 + options.search_half_width) * sw as f64) as usize;
    let max_ink = ink_s[lo..hi].iter().cloned().fold(1e-9, f64::max);

    let mut best = None;
    let mut best_score = f64::MIN;
    for x in lo..hi {
        let emptiness = 1.0 - ink_s[x] / max_ink; // 0..1
        let darkness = ((lum_wide[x] - lum_s[x]) / 40.0).clamp(0.0, 1.0); // 周りより暗い
        let center = 1.0 - ((x as f64 / sw as f64) - 0.5).abs() / options.search_half_width * 0.3; // 中央を少しだけ優先
        let score = (emptiness + darkness) * center;
        if score > best_score {
            best_score = score;
            best = Some(x);
        }
    }
    best.map(|x| ((x as f64 + 0.5) / scale).round() as u32)
}

/// 見開きを左右 2 枚に分ける。分割しない場合は 1 枚のまま返す。
pub fn split_spread(img: &RgbImage, options: &SplitOptions) -> (Vec<RgbImage>, SplitResult) {
    split_at(img, find_gutter(img, options))
}

/// 指定した x で左右に分ける (None や端なら分けない)。
pub fn split_at(img: &RgbImage, x: Option<u32>) -> (Vec<RgbImage>, SplitResult) {
    match x {
        Some(x) if x > 0 && x < img.width() => {
            let left = imageops::crop_imm(img, 0, 0, x, img.height()).to_image();
            let right = imageops::crop_imm(img, x, 0, img.width() - x, img.height()).to_image();
            (vec![left, right], SplitResult { gutter_x: Some(x) })
        }
        _ => (vec![img.clone()], SplitResult::default()),
    }
}

fn box_smooth(v: &[f64], r: usize) -> Vec<f64> {
    let n = v.len();
    let mut acc = vec![0f64; n + 1];
    for i in 0..n {
        acc[i + 1] = acc[i] + v[i];
    }
    (0..n)
        .map(|i| {
            let a = i.saturating_sub(r);
            let b = (i + r + 1).min(n);
            (acc[b] - acc[a]) / (b - a) as f64
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;
    use imageproc::drawing::draw_filled_rect_mut;
    use imageproc::rect::Rect;

    #[test]
    fn finds_gutter_between_two_text_pages() {
        let mut img = RgbImage::from_pixel(2000, 1400, Rgb([250, 250, 250]));
        for (x0, x1) in [(120, 900), (1180, 1880)] {
            for line in 0..30 {
                let mut x = x0;
                while x < x1 {
                    draw_filled_rect_mut(&mut img, Rect::at(x, 150 + line * 36).of_size(16, 20), Rgb([20, 20, 20]));
                    x += 22;
                }
            }
        }
        // ノドの影 (x = 1040 付近が暗い)
        for x in 1000..1080 {
            let d = (40 - (x as i32 - 1040).abs()) as u8 * 2;
            for y in 0..1400 {
                img.put_pixel(x, y, Rgb([250 - d, 250 - d, 250 - d]));
            }
        }
        let gx = find_gutter(&img, &SplitOptions::default()).unwrap();
        assert!((gx as i64 - 1040).abs() < 30, "{gx}");

        // 縦長の画像は分割しない
        let portrait = RgbImage::from_pixel(1000, 1400, Rgb([250, 250, 250]));
        assert!(find_gutter(&portrait, &SplitOptions::default()).is_none());
    }
}
