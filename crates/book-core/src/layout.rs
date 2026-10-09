//! 余白の統一。各ページの本文の外接矩形を求め、全ページを同じ大きさの紙面に揃えて切り出す。
//! C# 版のページ番号による位置合わせは使わない簡易版。
//!
//! - 出力ページの大きさ = 全ページの本文の幅・高さの最大値 + 余白
//! - 本文がページいっぱいにあるページ: 本文の左上を余白の位置に揃える
//! - 本文が少ないページ (章扉・章末など): 横は中央、縦は本文が上寄りなら上揃え、そうでなければ中央

use crate::imgutil;
use image::{imageops, Rgb, RgbImage};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutOptions {
    /// 余白 (出力ページの短辺に対する割合)
    pub margin_ratio: f64,
    /// これより暗い画素をインクとみなす (照明補正後の明るさ)
    pub ink_threshold: u8,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            margin_ratio: 0.05,
            ink_threshold: 160,
        }
    }
}

/// 本文 (インク) の外接矩形。インクがなければ None。
/// 外周 1.5% に接する成分 (紙の端の影・机など)、左右の端の縦長の筋、ごく小さなゴミは除く。
pub fn content_box(img: &RgbImage, options: &LayoutOptions) -> Option<Rect> {
    let gray = imgutil::to_gray(img);
    let (small, scale) = imgutil::shrink_to_max_side(&gray, 1000);
    let (w, h) = (small.width(), small.height());

    let mut ink = small.clone();
    for v in ink.as_mut().iter_mut() {
        *v = if *v < options.ink_threshold { 255 } else { 0 };
    }
    let (_, stats) = imgutil::components_with_stats(&ink);

    let bx = (w as f64 * 0.015).ceil() as u32;
    let by = (h as f64 * 0.015).ceil() as u32;
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for st in stats.iter().skip(1) {
        if st.area < 3 {
            continue;
        }
        let touches = st.left < bx || st.top < by || st.left + st.width > w - bx || st.top + st.height > h - by;
        if touches {
            continue;
        }
        // 紙の左右の端付近にある縦長の筋 (ノドの影・紙の縁の残り) は本文ではない
        let near_side = st.left + st.width < w * 12 / 100 || st.left > w * 88 / 100;
        if near_side && st.height > st.width * 15 {
            continue;
        }
        x0 = x0.min(st.left);
        y0 = y0.min(st.top);
        x1 = x1.max(st.left + st.width);
        y1 = y1.max(st.top + st.height);
    }
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let inv = 1.0 / scale;
    let fx0 = (x0 as f64 * inv).floor() as u32;
    let fy0 = (y0 as f64 * inv).floor() as u32;
    let fx1 = ((x1 as f64 * inv).ceil() as u32).min(img.width());
    let fy1 = ((y1 as f64 * inv).ceil() as u32).min(img.height());
    Some(Rect {
        x: fx0,
        y: fy0,
        w: fx1 - fx0,
        h: fy1 - fy0,
    })
}

/// 全ページで共通の出力サイズ (px) と、各ページの切り出し位置 (元画像の座標。はみ出してもよい) を決める。
pub fn plan_crops(pages: &[(u32, u32, Option<Rect>)], options: &LayoutOptions) -> ((u32, u32), Vec<(i64, i64)>) {
    let boxes: Vec<Rect> = pages.iter().filter_map(|p| p.2).collect();
    if boxes.is_empty() {
        // 本文がどこにもない: 最初のページの大きさのまま
        let (w, h) = pages.first().map(|p| (p.0, p.1)).unwrap_or((2480, 3508));
        return ((w, h), vec![(0, 0); pages.len()]);
    }

    let content_w = boxes.iter().map(|b| b.w).max().unwrap();
    let content_h = boxes.iter().map(|b| b.h).max().unwrap();
    let mut ws: Vec<f64> = boxes.iter().map(|b| b.w as f64).collect();
    let mut hs: Vec<f64> = boxes.iter().map(|b| b.h as f64).collect();
    let median_w = imgutil::quantile(&mut ws, 0.5).unwrap();
    let median_h = imgutil::quantile(&mut hs, 0.5).unwrap();

    let margin = ((content_w.min(content_h) as f64) * options.margin_ratio).round() as i64;
    let out_w = content_w as i64 + margin * 2;
    let out_h = content_h as i64 + margin * 2;

    let origins = pages
        .iter()
        .map(|&(pw, ph, b)| match b {
            None => ((pw as i64 - out_w) / 2, (ph as i64 - out_h) / 2),
            Some(b) => {
                let full_w = b.w as f64 >= median_w * 0.8;
                let full_h = b.h as f64 >= median_h * 0.8;
                let ox = if full_w {
                    b.x as i64 - margin
                } else {
                    b.x as i64 + b.w as i64 / 2 - out_w / 2
                };
                let upper = (b.y as f64 + b.h as f64 / 2.0) < ph as f64 * 0.4;
                let oy = if full_h || upper {
                    b.y as i64 - margin
                } else {
                    b.y as i64 + b.h as i64 / 2 - out_h / 2
                };
                (ox, oy)
            }
        })
        .collect();

    ((out_w as u32, out_h as u32), origins)
}

/// 本文の外接矩形 (少し広げたもの) の外側を白で塗る (ScanTailor の「余白を埋める」に相当)。
/// 影の取り残しや紙の縁が余白に残らないようにする。
pub fn fill_outside(img: &mut RgbImage, content: Rect, pad: u32) {
    let x0 = content.x.saturating_sub(pad);
    let y0 = content.y.saturating_sub(pad);
    let x1 = (content.x + content.w + pad).min(img.width());
    let y1 = (content.y + content.h + pad).min(img.height());
    for (x, y, p) in img.enumerate_pixels_mut() {
        if x < x0 || x >= x1 || y < y0 || y >= y1 {
            *p = Rgb([255, 255, 255]);
        }
    }
}

/// 元画像から (ox, oy) を左上とする out_w x out_h を切り出す。はみ出した部分は白。
pub fn crop_with_padding(img: &RgbImage, ox: i64, oy: i64, out_w: u32, out_h: u32) -> RgbImage {
    let mut out = RgbImage::from_pixel(out_w, out_h, Rgb([255, 255, 255]));
    let sx0 = ox.max(0);
    let sy0 = oy.max(0);
    let sx1 = (ox + out_w as i64).min(img.width() as i64);
    let sy1 = (oy + out_h as i64).min(img.height() as i64);
    if sx0 < sx1 && sy0 < sy1 {
        let part = imageops::crop_imm(img, sx0 as u32, sy0 as u32, (sx1 - sx0) as u32, (sy1 - sy0) as u32).to_image();
        imageops::replace(&mut out, &part, sx0 - ox, sy0 - oy);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_content_and_ignores_border_shadow() {
        let mut img = RgbImage::from_pixel(1000, 1400, Rgb([255, 255, 255]));
        // 本文
        for y in 200..1100 {
            for x in 150..800 {
                if (x / 10 + y / 14) % 2 == 0 {
                    img.put_pixel(x, y, Rgb([0, 0, 0]));
                }
            }
        }
        // 左端の影 (外周に接する)
        for y in 0..1400 {
            for x in 0..8 {
                img.put_pixel(x, y, Rgb([30, 30, 30]));
            }
        }
        let b = content_box(&img, &LayoutOptions::default()).unwrap();
        assert!((b.x as i64 - 150).abs() <= 3 && (b.y as i64 - 200).abs() <= 3, "{b:?}");
        assert!((b.w as i64 - 650).abs() <= 4 && (b.h as i64 - 900).abs() <= 4, "{b:?}");
    }

    #[test]
    fn crops_to_common_size() {
        let pages = vec![
            (
                1000,
                1400,
                Some(Rect {
                    x: 100,
                    y: 100,
                    w: 700,
                    h: 1100,
                }),
            ),
            (
                1000,
                1400,
                Some(Rect {
                    x: 200,
                    y: 150,
                    w: 690,
                    h: 1080,
                }),
            ),
            (
                1000,
                1400,
                Some(Rect {
                    x: 300,
                    y: 100,
                    w: 300,
                    h: 200,
                }),
            ), // 章扉
        ];
        let ((w, h), origins) = plan_crops(&pages, &LayoutOptions::default());
        let m = (700.0f64 * 0.05).round() as i64;
        assert_eq!((w as i64, h as i64), (700 + 2 * m, 1100 + 2 * m));
        assert_eq!(origins[0], (100 - m, 100 - m));
        assert_eq!(origins[1], (200 - m, 150 - m));
        // 章扉は横中央・上揃え
        assert_eq!(origins[2], (450 - w as i64 / 2, 100 - m));
    }
}
