//! 各処理で共通に使う小さな画像処理関数。
//! OpenCV に頼らず、必要なものだけを素直に実装している。

use image::{imageops, GrayImage, Luma, RgbImage};
use imageproc::region_labelling::{connected_components, Connectivity};

/// RGB をグレースケールにする。
pub fn to_gray(img: &RgbImage) -> GrayImage {
    imageops::grayscale(img)
}

/// 長辺が `max_side` 以下になるよう縮小する (拡大はしない)。戻り値の scale は 縮小後 / 元。
pub fn shrink_to_max_side(img: &GrayImage, max_side: u32) -> (GrayImage, f64) {
    let (w, h) = img.dimensions();
    let long = w.max(h);
    if long <= max_side {
        return (img.clone(), 1.0);
    }
    let s = max_side as f64 / long as f64;
    let nw = ((w as f64 * s).round() as u32).max(1);
    let nh = ((h as f64 * s).round() as u32).max(1);
    let out = imageops::resize(img, nw, nh, imageops::FilterType::Triangle);
    (out, nw as f64 / w as f64)
}

/// 積分画像による平均値の局所 2 値化 (OpenCV の ADAPTIVE_THRESH_MEAN_C + THRESH_BINARY_INV 相当)。
/// 局所平均 - c 以下の画素を 255 (インク) にする。
pub fn adaptive_threshold_inv(img: &GrayImage, block: u32, c: f64) -> GrayImage {
    let (w, h) = img.dimensions();
    let (wu, hu) = (w as usize, h as usize);
    let mut integral = vec![0u64; (wu + 1) * (hu + 1)];
    for y in 0..hu {
        let mut row = 0u64;
        for x in 0..wu {
            row += img.as_raw()[y * wu + x] as u64;
            integral[(y + 1) * (wu + 1) + x + 1] = integral[y * (wu + 1) + x + 1] + row;
        }
    }
    let r = (block / 2) as i64;
    let mut out = GrayImage::new(w, h);
    for y in 0..hu {
        // OpenCV と同様、境界では画像の端を複製した場合に近くなるよう、窓を画像内に切り詰めて平均する
        let y0 = (y as i64 - r).max(0) as usize;
        let y1 = ((y as i64 + r).min(hu as i64 - 1)) as usize + 1;
        for x in 0..wu {
            let x0 = (x as i64 - r).max(0) as usize;
            let x1 = ((x as i64 + r).min(wu as i64 - 1)) as usize + 1;
            let sum = integral[y1 * (wu + 1) + x1] + integral[y0 * (wu + 1) + x0] - integral[y0 * (wu + 1) + x1] - integral[y1 * (wu + 1) + x0];
            let mean = sum as f64 / ((x1 - x0) * (y1 - y0)) as f64;
            let v = img.as_raw()[y * wu + x] as f64;
            if v <= mean - c {
                out.as_mut()[y * wu + x] = 255;
            }
        }
    }
    out
}

/// 大津法で、しきい値以下の画素を 255 (インク) にする。
pub fn otsu_inv(img: &GrayImage) -> GrayImage {
    let level = imageproc::contrast::otsu_level(img);
    let mut out = GrayImage::new(img.width(), img.height());
    for (o, &v) in out.as_mut().iter_mut().zip(img.as_raw()) {
        if v <= level {
            *o = 255;
        }
    }
    out
}

/// 横方向の窓 (幅 kw) による 2 値画像のクロージング (膨張 → 収縮)。
pub fn close_horizontal(mask: &GrayImage, kw: u32) -> GrayImage {
    let dilated = horizontal_window(mask, kw, true);
    horizontal_window(&dilated, kw, false)
}

// 横方向の窓内に 1 つでも (dilate) / すべて (erode) 255 があれば 255 にする。
// 膨張の窓は OpenCV と同じく、アンカー = kw / 2 の位置 [x - a, x - a + kw - 1]。画像外は「影響なし」として扱う。
fn horizontal_window(mask: &GrayImage, kw: u32, dilate: bool) -> GrayImage {
    let (w, h) = mask.dimensions();
    let (wu, kw) = (w as i64, kw.max(1) as i64);
    let a = kw / 2;
    let mut out = GrayImage::new(w, h);
    let src = mask.as_raw();
    let dst = out.as_mut();
    for y in 0..h as usize {
        let row = &src[y * w as usize..(y + 1) * w as usize];
        // 累積和で窓内の 255 の数を数える
        let mut acc = vec![0i64; w as usize + 1];
        for x in 0..w as usize {
            acc[x + 1] = acc[x] + (row[x] != 0) as i64;
        }
        for x in 0..wu {
            // 収縮は膨張の窓を左右反転したものを使う (クロージングが元の画素を必ず含むように)
            let (lo, hi) = if dilate { (x - a, x - a + kw - 1) } else { (x - (kw - 1 - a), x + a) };
            let lo = lo.max(0);
            let hi = hi.min(wu - 1);
            let count = acc[hi as usize + 1] - acc[lo as usize];
            let on = if dilate { count > 0 } else { count == hi - lo + 1 };
            if on {
                dst[y * w as usize + x as usize] = 255;
            }
        }
    }
    out
}

/// 連結成分 1 つ分の統計 (OpenCV の connectedComponentsWithStats 相当)。
#[derive(Clone, Copy, Debug, Default)]
pub struct CompStat {
    pub left: u32,
    pub top: u32,
    pub width: u32,
    pub height: u32,
    pub area: u32,
}

/// 8 近傍の連結成分を求める。戻り値はラベル画像 (0 = 背景) と、ラベルごとの統計 (添字 0 は未使用)。
pub fn components_with_stats(mask: &GrayImage) -> (Vec<u32>, Vec<CompStat>) {
    let labels = connected_components(mask, Connectivity::Eight, Luma([0u8]));
    let (w, _) = mask.dimensions();
    let raw = labels.into_raw();
    let n = raw.iter().copied().max().unwrap_or(0) as usize + 1;
    let mut minx = vec![u32::MAX; n];
    let mut miny = vec![u32::MAX; n];
    let mut maxx = vec![0u32; n];
    let mut maxy = vec![0u32; n];
    let mut area = vec![0u32; n];
    for (i, &l) in raw.iter().enumerate() {
        if l == 0 {
            continue;
        }
        let l = l as usize;
        let x = i as u32 % w;
        let y = i as u32 / w;
        minx[l] = minx[l].min(x);
        miny[l] = miny[l].min(y);
        maxx[l] = maxx[l].max(x);
        maxy[l] = maxy[l].max(y);
        area[l] += 1;
    }
    let stats = (0..n)
        .map(|l| {
            if area[l] == 0 {
                CompStat::default()
            } else {
                CompStat {
                    left: minx[l],
                    top: miny[l],
                    width: maxx[l] - minx[l] + 1,
                    height: maxy[l] - miny[l] + 1,
                    area: area[l],
                }
            }
        })
        .collect();
    (raw, stats)
}

/// f32 の 2 次元配列 (行優先、幅 w) に、分離型ガウスぼかしをかける (端は複製)。
pub fn gaussian_blur_f32(data: &mut [f32], w: usize, h: usize, sigma: f64) {
    if sigma <= 0.0 || w == 0 || h == 0 {
        return;
    }
    let r = (sigma * 3.0).ceil() as i64;
    let kernel: Vec<f64> = (-r..=r).map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp()).collect();
    let ksum: f64 = kernel.iter().sum();
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for (k, kv) in kernel.iter().enumerate() {
                let xx = (x as i64 + k as i64 - r).clamp(0, w as i64 - 1) as usize;
                s += data[y * w + xx] as f64 * kv;
            }
            tmp[y * w + x] = (s / ksum) as f32;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for (k, kv) in kernel.iter().enumerate() {
                let yy = (y as i64 + k as i64 - r).clamp(0, h as i64 - 1) as usize;
                s += tmp[yy * w + x] as f64 * kv;
            }
            data[y * w + x] = (s / ksum) as f32;
        }
    }
}

/// f32 の 2 次元配列を、範囲外は端の値として双一次補間で引く。
pub fn sample_bilinear_f32(data: &[f32], w: usize, h: usize, x: f64, y: f64) -> f64 {
    let x = x.clamp(0.0, (w - 1) as f64);
    let y = y.clamp(0.0, (h - 1) as f64);
    let x0 = (x.floor() as usize).min(w.saturating_sub(2));
    let y0 = (y.floor() as usize).min(h.saturating_sub(2));
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = x - x0 as f64;
    let fy = y - y0 as f64;
    let a = data[y0 * w + x0] as f64 * (1.0 - fx) + data[y0 * w + x1] as f64 * fx;
    let b = data[y1 * w + x0] as f64 * (1.0 - fx) + data[y1 * w + x1] as f64 * fx;
    a * (1.0 - fy) + b * fy
}

/// 1 次元の最大値フィルタ (窓幅 2r+1、端は窓を切り詰める)。単調キューで O(n)。
pub fn max_filter_1d(src: &[u8], r: usize, out: &mut [u8]) {
    let n = src.len();
    let mut dq: std::collections::VecDeque<usize> = std::collections::VecDeque::new();
    let mut next = 0;
    for i in 0..n {
        let hi = (i + r).min(n - 1);
        while next <= hi {
            while let Some(&b) = dq.back() {
                if src[b] <= src[next] {
                    dq.pop_back();
                } else {
                    break;
                }
            }
            dq.push_back(next);
            next += 1;
        }
        let lo = i.saturating_sub(r);
        while let Some(&f) = dq.front() {
            if f < lo {
                dq.pop_front();
            } else {
                break;
            }
        }
        out[i] = src[*dq.front().unwrap()];
    }
}

/// グレースケール画像の 2 次元最大値フィルタ (正方形の窓、半径 r)。
pub fn max_filter(img: &GrayImage, r: usize) -> GrayImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut tmp = vec![0u8; w * h];
    for y in 0..h {
        max_filter_1d(&img.as_raw()[y * w..(y + 1) * w], r, &mut tmp[y * w..(y + 1) * w]);
    }
    let mut out = GrayImage::new(w as u32, h as u32);
    let mut col = vec![0u8; h];
    let mut col_out = vec![0u8; h];
    for x in 0..w {
        for y in 0..h {
            col[y] = tmp[y * w + x];
        }
        max_filter_1d(&col, r, &mut col_out);
        for y in 0..h {
            out.as_mut()[y * w + x] = col_out[y];
        }
    }
    out
}

/// 配列の分位点 (q = 0..1)。空なら None。
pub fn quantile(values: &mut [f64], q: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let i = ((values.len() - 1) as f64 * q).round() as usize;
    Some(values[i])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_horizontal_fills_small_gaps() {
        let mut m = GrayImage::new(10, 1);
        m.put_pixel(2, 0, Luma([255]));
        m.put_pixel(5, 0, Luma([255]));
        let c = close_horizontal(&m, 4);
        let v: Vec<u8> = c.as_raw().clone();
        assert_eq!(&v[2..=5], &[255, 255, 255, 255]);
        assert_eq!(v[0], 0);
        assert_eq!(v[8], 0);
    }

    #[test]
    fn max_filter_1d_matches_naive() {
        let src = [3u8, 1, 4, 1, 5, 9, 2, 6, 5, 3, 5];
        let mut out = [0u8; 11];
        max_filter_1d(&src, 2, &mut out);
        for i in 0..src.len() {
            let lo = i.saturating_sub(2);
            let hi = (i + 2).min(src.len() - 1);
            assert_eq!(out[i], *src[lo..=hi].iter().max().unwrap());
        }
    }
}
