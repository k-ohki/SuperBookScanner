//! 影・照明ムラの除去。
//! 紙の明るさ (背景) を推定し、各画素をそれで割って紙を白に揃える。ノドの影や撮影時の照明ムラもここで消える。
//! (ScanTailor の Output 段階の「照明の均一化」に相当する処理)

use crate::imgutil;
use image::{imageops, GrayImage, RgbImage};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct IlluminationOptions {
    /// 背景推定に使う縮小画像の長辺
    pub analysis_max_size: u32,
    /// 背景推定で文字を消すための最大値フィルタの半径 (縮小画像の長辺に対する割合)
    pub text_removal_ratio: f64,
    /// 背景をなめらかにするぼかしの強さ (縮小画像の長辺に対する割合)
    pub smooth_ratio: f64,
    /// 補正後、この値以上の明るさは紙とみなして真っ白にする (0-255)
    pub white_point: u8,
}

impl Default for IlluminationOptions {
    fn default() -> Self {
        Self {
            analysis_max_size: 800,
            text_removal_ratio: 0.012,
            smooth_ratio: 0.02,
            white_point: 235,
        }
    }
}

/// 背景 (紙の明るさ) を割り算して、照明ムラと影を取り除く。
/// 色ごとに背景を推定するので、黄ばんだ紙や電球色の照明も白い紙に揃う。
pub fn normalize_illumination(img: &RgbImage, options: &IlluminationOptions) -> RgbImage {
    let bg: Vec<GrayImage> = (0..3).map(|c| estimate_background(&channel(img, c), options)).collect();

    let wp = options.white_point.max(1) as f32;
    let mut out = RgbImage::new(img.width(), img.height());
    for (i, (o, p)) in out.pixels_mut().zip(img.pixels()).enumerate() {
        for c in 0..3 {
            let b = (bg[c].as_raw()[i] as f32).max(16.0);
            // 紙 = 255 になるよう割り算し、さらに white_point 以上を白に飛ばす
            let v = p[c] as f32 * 255.0 / b;
            o[c] = (v * 255.0 / wp).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn channel(img: &RgbImage, c: usize) -> GrayImage {
    GrayImage::from_raw(img.width(), img.height(), img.pixels().map(|p| p[c]).collect()).unwrap()
}

// 1 チャンネル分の背景 (紙の明るさ) を、元の大きさで推定する
fn estimate_background(ch: &GrayImage, options: &IlluminationOptions) -> GrayImage {
    let (small, _) = imgutil::shrink_to_max_side(ch, options.analysis_max_size);
    let long = small.width().max(small.height()) as f64;

    // 文字 (暗い) を最大値フィルタで消し、紙の明るさだけを残す
    let r = ((long * options.text_removal_ratio).round() as usize).max(2);
    let bg_small = imgutil::max_filter(&small, r);

    // なめらかにする
    let (sw, sh) = (bg_small.width() as usize, bg_small.height() as usize);
    let mut bgf: Vec<f32> = bg_small.as_raw().iter().map(|&v| v as f32).collect();
    imgutil::gaussian_blur_f32(&mut bgf, sw, sh, long * options.smooth_ratio);
    let bg_small = GrayImage::from_raw(sw as u32, sh as u32, bgf.iter().map(|&v| v.round().clamp(1.0, 255.0) as u8).collect()).unwrap();

    imageops::resize(&bg_small, ch.width(), ch.height(), imageops::FilterType::Triangle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn removes_gradient_shadow_but_keeps_text() {
        // 左端に向かって暗くなる (ノドの影) 紙に、黒い文字 (四角) がある画像
        let mut img = RgbImage::new(600, 800);
        for (x, _, p) in img.enumerate_pixels_mut() {
            let v = (120.0 + 120.0 * (x as f32 / 600.0)) as u8;
            *p = Rgb([v, v, v]);
        }
        for y in 300..320 {
            for x in 100..120 {
                img.put_pixel(x, y, Rgb([20, 20, 20]));
            }
        }
        let out = normalize_illumination(&img, &IlluminationOptions::default());
        // 影の部分も右端も、紙はほぼ白になる
        assert!(out.get_pixel(30, 100)[0] > 240, "{:?}", out.get_pixel(30, 100));
        assert!(out.get_pixel(570, 100)[0] > 240);
        // 文字は暗いまま
        assert!(out.get_pixel(110, 310)[0] < 80, "{:?}", out.get_pixel(110, 310));
    }

    #[test]
    fn yellowish_paper_becomes_white() {
        let img = RgbImage::from_pixel(400, 600, Rgb([240, 225, 190]));
        let out = normalize_illumination(&img, &IlluminationOptions::default());
        assert_eq!(*out.get_pixel(200, 300), Rgb([255, 255, 255]));
    }
}
