//! 本の綴じ目 (ノド) 付近の湾曲 (行の曲がり) を補正する。
//!
//! C# 版 `BookDewarp.cs` (PR #1、C# 版はリポジトリから削除済み) の Rust 移植。アルゴリズムは同じで、
//! ScanTailor の dewarping の考え方 (テキスト行のトレース → 歪みモデル → 面の展開) を参考にした独自実装。
//!
//! 処理の流れ:
//!   1. 解析用に縮小したグレー画像を 2 値化し、文字らしい連結成分だけを残す
//!   2. 文字を横方向に連結してテキスト行を作り、各行の中心線を多項式で近似する。
//!      多項式では表せない急な段差 (紙の折れ、平面化の誤り) は、多項式との差の移動中央値で上乗せする
//!   3. 各行について「平らな部分」に頑健に直線を当てはめ、曲線と直線の差を縦方向の変位とする
//!      (縦書きなどで行が取れない場合は、本文ブロックの上端・下端の包絡線を代わりに使う)
//!   4. 複数の行の変位を y 方向に補間して、ページ全体の変位場 D(x, y) を作る
//!   5. ノド付近の奥行きによる横方向の縮みを、変位プロファイルの弧長で近似して伸ばす
//!   6. 画像を展開する (双三次補間)

use crate::imgutil::{self, CompStat};
use image::{GrayImage, Rgb, RgbImage};
use imageproc::geometric_transformations::{warp_into_with, Border, Interpolation};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct DewarpOptions {
    /// 解析用縮小画像の長辺の最大ピクセル数
    pub analysis_max_size: u32,
    /// テキスト行を近似する多項式の次数
    pub poly_degree: usize,
    /// テキスト行として採用する最小の幅 (ページ幅に対する割合)
    pub min_line_width_ratio: f64,
    /// テキスト行による補正を行うのに必要な最小の行数
    pub min_lines: usize,
    /// 変位の上限 (ページ高さに対する割合)。これを超える変位は打ち切る
    pub max_displacement_ratio: f64,
    /// 最大変位がこれ (ページ高さに対する割合) 未満なら、歪みなしとみなして何もしない
    pub min_effect_ratio: f64,
    /// ノド付近の横方向の縮みを補正する
    pub correct_horizontal: bool,
    /// 横方向補正の強さ (1.0 = 縦変位をそのまま奥行きとみなす)
    pub horizontal_strength: f64,
    /// テキスト行が少ない場合 (縦書き等) に本文の上端・下端の包絡線を使う
    pub use_envelope_fallback: bool,
}

impl Default for DewarpOptions {
    fn default() -> Self {
        Self {
            analysis_max_size: 2000,
            poly_degree: 4,
            min_line_width_ratio: 0.2,
            min_lines: 3,
            max_displacement_ratio: 0.08,
            min_effect_ratio: 0.003,
            correct_horizontal: true,
            horizontal_strength: 1.0,
            use_envelope_fallback: true,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DewarpResult {
    pub applied: bool,
    pub message: String,
    pub num_line_curves: usize,
    pub num_envelope_curves: usize,
    pub max_displacement_px: f64,
    pub output_width: u32,
    pub output_height: u32,
}

/// 変位場のグリッド間隔 (解析座標)
const GRID_CELL: usize = 8;

// 1 本の曲線 (解析座標系)
#[derive(Clone, Debug)]
struct Curve {
    poly: Vec<f64>, // u = (x - cx) / cx の多項式
    line_a: f64,    // 直線 y = A + B x (曲がっていない場合の位置)
    line_b: f64,
    x_min: f64,
    x_max: f64,
    slope_min: f64, // 両端での変位の傾き (外挿用)
    slope_max: f64,
    disp_min: f64, // 両端での変位
    disp_max: f64,
    is_envelope: bool,
    cx: f64,
    /// 多項式からのずれ (行の実際の形)。等間隔 (detail_step) に並べた値。空なら多項式だけを使う
    detail: Vec<f64>,
    detail_x0: f64,
    detail_step: f64,
}

impl Curve {
    fn u(&self, x: f64) -> f64 {
        (x - self.cx) / self.cx
    }
    fn y(&self, x: f64) -> f64 {
        poly_eval(&self.poly, self.u(x)) + self.detail_at(x)
    }
    /// 多項式だけの値 (範囲外への外挿の傾きに使う。段差の局所的な傾きで外挿しないため)
    fn y_smooth(&self, x: f64) -> f64 {
        poly_eval(&self.poly, self.u(x))
    }
    fn detail_at(&self, x: f64) -> f64 {
        if self.detail.is_empty() {
            return 0.0;
        }
        let f = ((x - self.detail_x0) / self.detail_step).clamp(0.0, (self.detail.len() - 1) as f64);
        let i = (f as usize).min(self.detail.len().saturating_sub(2));
        let t = f - i as f64;
        if i + 1 >= self.detail.len() {
            return self.detail[i];
        }
        self.detail[i] * (1.0 - t) + self.detail[i + 1] * t
    }
    fn target(&self, x: f64) -> f64 {
        self.line_a + self.line_b * x
    }
    fn disp(&self, x: f64, max_disp: f64) -> f64 {
        let d = if x < self.x_min {
            self.disp_min + self.slope_min * (x - self.x_min)
        } else if x > self.x_max {
            self.disp_max + self.slope_max * (x - self.x_max)
        } else {
            self.y(x) - self.target(x)
        };
        d.clamp(-max_disp, max_disp)
    }
    fn distance_outside(&self, x: f64) -> f64 {
        if x < self.x_min {
            self.x_min - x
        } else if x > self.x_max {
            x - self.x_max
        } else {
            0.0
        }
    }
}

/// 画像の綴じ目付近の湾曲を補正した新しい画像を返す。
/// 補正不要・補正不能と判断した場合は元画像の複製を返す (result.applied == false)。
pub fn dewarp(src: &RgbImage, options: &DewarpOptions) -> (RgbImage, DewarpResult) {
    let (sw, sh) = src.dimensions();
    let mut result = DewarpResult {
        output_width: sw,
        output_height: sh,
        ..Default::default()
    };

    if sw < 64 || sh < 64 {
        result.message = "Image too small".into();
        return (src.clone(), result);
    }

    // ---- 1. 解析用画像 ----
    let gray = imgutil::to_gray(src);
    let (small, _) = imgutil::shrink_to_max_side(&gray, options.analysis_max_size);
    let scale = small.width() as f64 / sw as f64;
    let (w, h) = (small.width() as usize, small.height() as usize);

    let (char_mask, char_height, num_chars) = build_char_mask(&small);
    if num_chars < 30 || char_height <= 0.0 {
        result.message = "Not enough text".into();
        return (src.clone(), result);
    }

    let max_disp = options.max_displacement_ratio * h as f64;

    // ---- 2-3. テキスト行の曲線 ----
    let mut curves = detect_line_curves(&char_mask, char_height, options);
    result.num_line_curves = curves.len();

    if curves.len() < options.min_lines && options.use_envelope_fallback {
        let env = detect_envelope_curves(&char_mask, char_height, options);
        result.num_envelope_curves = env.len();
        curves.extend(env);
    }

    if curves.len() < 2 || (result.num_envelope_curves == 0 && curves.len() < options.min_lines) {
        result.message = "Not enough text lines".into();
        return (src.clone(), result);
    }

    // ---- 4. 変位場 D(x, y) (解析座標、出力側 y で引く) ----
    let gw = w.div_ceil(GRID_CELL) + 1;
    let gh = h.div_ceil(GRID_CELL) + 1;
    let disp_grid = build_displacement_grid(&curves, w, gw, gh, max_disp);

    let max_abs = disp_grid.iter().fold(0f64, |m, &v| m.max((v as f64).abs()));
    result.max_displacement_px = max_abs / scale;

    if max_abs < options.min_effect_ratio * h as f64 {
        result.message = "No significant curvature".into();
        return (src.clone(), result);
    }

    // ---- 5. 横方向 (弧長) ----
    let s_of_x = build_arc_length(&disp_grid, &curves, gw, gh, options); // 長さ gw、解析座標
    let total_s = s_of_x[gw - 1] * w as f64 / ((gw - 1) * GRID_CELL) as f64;
    let out_w = ((total_s / scale).round() as u32).max(1);
    let out_h = sh;

    // ---- 6. 出力座標グリッド上でのソース座標オフセット (フル解像度の px) ----
    let ogw = ((out_w as f64 * scale) / GRID_CELL as f64).ceil() as usize + 1;
    let mut off_x = vec![0f32; gh * ogw];
    let mut off_y = vec![0f32; gh * ogw];
    for i in 0..ogw {
        let s = (i * GRID_CELL) as f64; // 出力 x (解析座標) = 弧長
        let xa = invert_monotone(&s_of_x, s); // ソース x (解析座標)
        let gx = (xa / GRID_CELL as f64).clamp(0.0, (gw - 1) as f64);
        let gx0 = (gx as usize).min(gw - 2);
        let fx = gx - gx0 as f64;
        for j in 0..gh {
            let d = disp_grid[j * gw + gx0] as f64 * (1.0 - fx) + disp_grid[j * gw + gx0 + 1] as f64 * fx;
            off_x[j * ogw + i] = ((xa - s) / scale) as f32;
            off_y[j * ogw + i] = (d / scale) as f32;
        }
    }

    // グリッドの節点 (i, j) は、出力のフル解像度で (i, j) * GRID_CELL / scale の位置にある
    let grid_step = GRID_CELL as f64 / scale;
    let mapping = |x: f32, y: f32| -> (f32, f32) {
        let gx = x as f64 / grid_step;
        let gy = y as f64 / grid_step;
        let dx = imgutil::sample_bilinear_f32(&off_x, ogw, gh, gx, gy);
        let dy = imgutil::sample_bilinear_f32(&off_y, ogw, gh, gx, gy);
        (x + dx as f32, y + dy as f32)
    };

    let mut dst = RgbImage::new(out_w, out_h);
    warp_into_with(src, mapping, Interpolation::Bicubic, Border::Replicate, &mut dst);

    result.applied = true;
    result.output_width = out_w;
    result.output_height = out_h;
    result.message = "OK".into();
    (dst, result)
}

// ------------------------------------------------------------------
// 文字マスク
// ------------------------------------------------------------------
fn build_char_mask(small: &GrayImage) -> (GrayImage, f64, usize) {
    let (w, h) = (small.width() as usize, small.height() as usize);

    let block = (((w.min(h) / 30) | 1) as u32).max(15);
    let adaptive = imgutil::adaptive_threshold_inv(small, block, 12.0);

    // 一様に暗いノドの影などは、大津法で「インク」とされても局所適応では拾わないので AND を取る
    let otsu = imgutil::otsu_inv(small);
    let mut bin = adaptive;
    for (b, o) in bin.as_mut().iter_mut().zip(otsu.as_raw()) {
        *b &= *o;
    }

    // 外周 (スキャンの枠など) を除去
    let bx = ((w as f64 * 0.015) as usize).max(2);
    let by = ((h as f64 * 0.015) as usize).max(2);
    for y in 0..h {
        for x in 0..w {
            if y < by || y >= h - by || x < bx || x >= w - bx {
                bin.as_mut()[y * w + x] = 0;
            }
        }
    }

    let (labels, stats) = imgutil::components_with_stats(&bin);

    let mut keep = vec![false; stats.len()];
    let mut heights = Vec::new();
    let max_h = (h / 15).max(6) as u32;
    let max_w = (w / 8).max(6) as u32;
    for (i, st) in stats.iter().enumerate().skip(1) {
        let CompStat {
            width: cw, height: ch, area, ..
        } = *st;
        if ch < 2 || area < 4 || ch > max_h || cw > max_w {
            continue;
        }
        // 細長すぎる成分 (罫線・影の縁) は文字ではない
        if cw > ch * 12 || ch > cw * 12 {
            continue;
        }
        keep[i] = true;
        if ch >= 3 {
            heights.push(ch);
        }
    }

    let num_chars = heights.len();
    let mut char_height = 0.0;
    if !heights.is_empty() {
        heights.sort_unstable();
        // 句読点などの小さい成分に引きずられないよう、上位側の中央値を使う
        char_height = heights[(heights.len() as f64 * 0.65) as usize] as f64;
    }

    let mut mask = GrayImage::new(w as u32, h as u32);
    for (m, &l) in mask.as_mut().iter_mut().zip(&labels) {
        if keep[l as usize] {
            *m = 255;
        }
    }
    (mask, char_height, num_chars)
}

// ------------------------------------------------------------------
// テキスト行の検出と近似
// ------------------------------------------------------------------
fn detect_line_curves(char_mask: &GrayImage, char_height: f64, options: &DewarpOptions) -> Vec<Curve> {
    let (w, h) = (char_mask.width() as usize, char_mask.height() as usize);
    let mut curves = Vec::new();

    let kw = ((char_height * 1.3).round() as u32).max(3);
    let smeared = imgutil::close_horizontal(char_mask, kw);
    let (labels, stats) = imgutil::components_with_stats(&smeared);

    let mut cand_index = vec![-1i64; stats.len()];
    let mut cands: Vec<(usize, usize)> = Vec::new(); // (x, w)
    for (i, st) in stats.iter().enumerate().skip(1) {
        let (cx, cw, ch) = (st.left as usize, st.width as usize, st.height as f64);
        if st.area == 0 {
            continue;
        }
        if (cw as f64) < options.min_line_width_ratio * w as f64 {
            continue;
        }
        if (cw as f64) < ch * 6.0 {
            continue;
        }
        // 複数行が繋がったものは除外 (ただし湾曲で背が高くなる分は許す)
        if ch > char_height * 2.5 + cw as f64 * 0.06 {
            continue;
        }
        cand_index[i] = cands.len() as i64;
        cands.push((cx, cw));
    }
    if cands.is_empty() {
        return curves;
    }

    let mut sum_y: Vec<Vec<f64>> = cands.iter().map(|c| vec![0.0; c.1]).collect();
    let mut cnt: Vec<Vec<u32>> = cands.iter().map(|c| vec![0; c.1]).collect();
    for y in 0..h {
        for x in 0..w {
            let l = labels[y * w + x] as usize;
            if l == 0 {
                continue;
            }
            let ci = cand_index[l];
            if ci < 0 {
                continue;
            }
            let ci = ci as usize;
            let lx = x - cands[ci].0;
            sum_y[ci][lx] += y as f64;
            cnt[ci][lx] += 1;
        }
    }

    let cx = w as f64 / 2.0;
    for ci in 0..cands.len() {
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        for lx in 0..cands[ci].1 {
            if cnt[ci][lx] == 0 {
                continue;
            }
            xs.push((cands[ci].0 + lx) as f64);
            ys.push(sum_y[ci][lx] / cnt[ci][lx] as f64);
        }
        if (xs.len() as f64) < cands[ci].1 as f64 * 0.6 {
            continue;
        }

        let span = (xs[xs.len() - 1] - xs[0]) / w as f64;
        let degree = if span < 0.35 { 2 } else { options.poly_degree };

        if let Some(mut c) = fit_curve(&xs, &ys, degree, cx, char_height * 0.35, false, 0.0, 0.0) {
            add_detail(&mut c, &xs, &ys, char_height);
            curves.push(c);
        }
    }
    curves
}

/// 曲線の「平らな部分」に頑健に直線を当てはめ (ノド側の曲がった部分を外れ値として除く)、
/// 端点での変位と傾きを求める。
fn fit_target_line(c: &mut Curve) -> Option<()> {
    let n = 200usize;
    let sx: Vec<f64> = (0..n).map(|i| c.x_min + (c.x_max - c.x_min) * i as f64 / (n - 1) as f64).collect();
    let sy: Vec<f64> = sx.iter().map(|&x| c.y(x)).collect();
    let mut wts = vec![1.0; sx.len()];
    let (mut a, mut b) = (0.0, 0.0);
    for _ in 0..6 {
        match weighted_line_fit(&sx, &sy, &wts) {
            Some(r) => (a, b) = r,
            None => return None,
        }
        let res: Vec<f64> = sx.iter().zip(&sy).map(|(x, y)| (y - (a + b * x)).abs()).collect();
        let mut sorted = res.clone();
        sorted.sort_by(|p, q| p.partial_cmp(q).unwrap());
        let med = sorted[sorted.len() / 2];
        let th = (med * 1.5).max(0.5);
        for (wt, r) in wts.iter_mut().zip(&res) {
            *wt = if *r <= th { 1.0 } else { 0.0 };
        }
        if wts.iter().sum::<f64>() < sx.len() as f64 * 0.3 {
            break;
        }
    }
    c.line_a = a;
    c.line_b = b;

    // 端点での変位と傾き (範囲外への外挿用、端の 3% の割線。傾きは多項式だけから求める)
    let edge = ((c.x_max - c.x_min) * 0.03).max(2.0);
    c.disp_min = c.y(c.x_min) - c.target(c.x_min);
    c.disp_max = c.y(c.x_max) - c.target(c.x_max);
    let smooth = |x: f64| c.y_smooth(x) - c.target(x);
    let slope_min = (smooth(c.x_min) - smooth(c.x_min + edge)) / -edge;
    let slope_max = (smooth(c.x_max) - smooth(c.x_max - edge)) / edge;
    c.slope_min = slope_min;
    c.slope_max = slope_max;
    Some(())
}

/// 多項式では表せない段差を、多項式との差の移動中央値として曲線に加え、目標の直線を当てはめ直す。
/// 中央値は段差 (折れ目) を保ち、ルビなどで短く盛り上がったところは無視する。
fn add_detail(c: &mut Curve, xs: &[f64], ys: &[f64], char_height: f64) {
    if xs.len() < 8 {
        return;
    }
    let res: Vec<f64> = xs.iter().zip(ys).map(|(&x, &y)| y - poly_eval(&c.poly, c.u(x))).collect();
    let half = ((char_height * 2.0).round() as usize).max(2);
    let step = (char_height / 2.0).max(1.0);
    let (x0, x1) = (xs[0], xs[xs.len() - 1]);
    let n = ((x1 - x0) / step).floor() as usize + 1;
    let mut detail: Vec<f64> = Vec::with_capacity(n);
    let mut k = 0;
    let mut window: Vec<f64> = Vec::new();
    for i in 0..n {
        let x = x0 + i as f64 * step;
        while k < xs.len() && xs[k] < x {
            k += 1;
        }
        let lo = k.saturating_sub(half);
        let hi = (k + half).min(xs.len());
        window.clear();
        window.extend(res[lo..hi].iter().filter(|r| r.abs() < char_height));
        if window.is_empty() {
            detail.push(0.0);
            continue;
        }
        window.sort_by(|a, b| a.total_cmp(b));
        detail.push(window[window.len() / 2]);
    }
    // 小さな揺れ (文字の形による) は捨て、段差だけを残す
    if detail.iter().fold(0f64, |m, d| m.max(d.abs())) < char_height * 0.15 {
        return;
    }
    c.detail = detail;
    c.detail_x0 = x0;
    c.detail_step = step;
    fit_target_line(c);
}

// 縦書き等のための、本文ブロック上端・下端の包絡線
fn detect_envelope_curves(char_mask: &GrayImage, char_height: f64, options: &DewarpOptions) -> Vec<Curve> {
    let (w, h) = (char_mask.width() as usize, char_mask.height() as usize);
    let mut curves = Vec::new();

    // 字形による上端・下端のばらつきを抑えるため、隣の文字・行間を横につないでから包絡線を取る
    let kw = ((char_height * 2.5).round() as u32).max(3);
    let joined = imgutil::close_horizontal(char_mask, kw);
    let m = joined.as_raw();

    // 列ごとの最上部・最下部の文字画素
    let (mut top_xs, mut top_ys, mut bot_xs, mut bot_ys) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for x in 0..w {
        let Some(top) = (0..h).find(|&y| m[y * w + x] != 0) else { continue };
        let bot = (0..h).rev().find(|&y| m[y * w + x] != 0).unwrap();
        top_xs.push(x as f64);
        top_ys.push(top as f64);
        bot_xs.push(x as f64);
        bot_ys.push(bot as f64);
    }
    if (top_xs.len() as f64) < w as f64 * 0.3 {
        return curves;
    }

    let cx = w as f64 / 2.0;
    for (xs, ys) in [(&top_xs, &top_ys), (&bot_xs, &bot_ys)] {
        // 柱やノンブルなど一部だけ飛び出したものは外れ値として除く
        if let Some(mut c) = fit_curve(xs, ys, options.poly_degree.min(3), cx, char_height * 0.6, true, char_height * 1.5, 0.5) {
            c.is_envelope = true;
            if c.x_max - c.x_min >= w as f64 * 0.4 {
                curves.push(c);
            }
        }
    }
    curves
}

#[allow(clippy::too_many_arguments)]
fn fit_curve(xs: &[f64], ys: &[f64], degree: usize, cx: f64, max_rms: f64, robust: bool, outlier_threshold: f64, min_inlier_ratio: f64) -> Option<Curve> {
    let mut px = xs.to_vec();
    let mut py = ys.to_vec();
    let mut poly: Option<Vec<f64>> = None;
    let iterations = if robust { 10 } else { 1 };

    for it in 0..iterations {
        if px.len() < degree + 5 {
            return None;
        }
        let us: Vec<f64> = px.iter().map(|x| (x - cx) / cx).collect();
        let p = poly_fit(&us, &py, degree)?;
        poly = Some(p.clone());
        if !robust || it == iterations - 1 {
            break;
        }

        // しきい値は残差の中央値から徐々に outlier_threshold まで絞る (外れ値に引っ張られた初回の当てはめ対策)
        let res: Vec<f64> = px.iter().zip(&py).map(|(x, y)| (poly_eval(&p, (x - cx) / cx) - y).abs()).collect();
        let mut sorted = res.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let th = outlier_threshold.max(2.0 * sorted[sorted.len() / 2]);

        let (mut nx, mut ny) = (Vec::new(), Vec::new());
        for i in 0..px.len() {
            if res[i] <= th {
                nx.push(px[i]);
                ny.push(py[i]);
            }
        }
        if nx.len() == px.len() && th <= outlier_threshold {
            break;
        }
        px = nx;
        py = ny;
    }
    let poly = poly?;
    if (px.len() as f64) < xs.len() as f64 * min_inlier_ratio {
        return None;
    }

    let ss: f64 = px.iter().zip(&py).map(|(x, y)| (poly_eval(&poly, (x - cx) / cx) - y).powi(2)).sum();
    let rms = (ss / px.len() as f64).sqrt();
    if rms > max_rms {
        return None;
    }

    let mut curve = Curve {
        poly,
        cx,
        x_min: px[0],
        x_max: px[px.len() - 1],
        line_a: 0.0,
        line_b: 0.0,
        slope_min: 0.0,
        slope_max: 0.0,
        disp_min: 0.0,
        disp_max: 0.0,
        is_envelope: false,
        detail: Vec::new(),
        detail_x0: 0.0,
        detail_step: 1.0,
    };

    fit_target_line(&mut curve)?;
    Some(curve)
}

// ------------------------------------------------------------------
// 変位場
// ------------------------------------------------------------------
fn build_displacement_grid(curves: &[Curve], w: usize, gw: usize, gh: usize, max_disp: f64) -> Vec<f32> {
    let mut grid = vec![0f32; gw * gh];
    let tol = (GRID_CELL * 2) as f64;
    let mut items: Vec<(f64, f64)> = Vec::new();

    for i in 0..gw {
        let x = (i * GRID_CELL) as f64;

        // その x を覆う曲線を使う。どれも覆わなければ (ノドの余白など)、最も近い端の曲線を外挿する
        let mut used: Vec<&Curve> = curves.iter().filter(|c| c.distance_outside(x) <= tol).collect();
        if used.is_empty() {
            let min_dist = curves.iter().map(|c| c.distance_outside(x)).fold(f64::MAX, f64::min);
            used = curves.iter().filter(|c| c.distance_outside(x) <= min_dist + w as f64 * 0.05).collect();
        }

        items.clear();
        items.extend(used.iter().map(|c| (c.target(x), c.disp(x, max_disp))));
        items.sort_by(|p, q| p.0.partial_cmp(&q.0).unwrap());

        let mut k = 0;
        for j in 0..gh {
            let y = (j * GRID_CELL) as f64;
            let d = if y <= items[0].0 {
                items[0].1
            } else if y >= items[items.len() - 1].0 {
                items[items.len() - 1].1
            } else {
                while k < items.len() - 2 && items[k + 1].0 < y {
                    k += 1;
                }
                let (p, q) = (items[k], items[k + 1]);
                let f = if q.0 - p.0 < 1e-6 { 0.5 } else { (y - p.0) / (q.0 - p.0) };
                p.1 + (q.1 - p.1) * f
            };
            grid[j * gw + i] = d as f32;
        }
    }

    // 曲線の入れ替わりによる段差をならす
    imgutil::gaussian_blur_f32(&mut grid, gw, gh, 1.5);
    grid
}

// 横方向: 本文範囲の平均変位を奥行きプロファイルとみなし、その弧長で x を伸ばす
fn build_arc_length(disp_grid: &[f32], curves: &[Curve], gw: usize, gh: usize, options: &DewarpOptions) -> Vec<f64> {
    let mut s: Vec<f64> = (0..gw).map(|i| (i * GRID_CELL) as f64).collect();
    if !options.correct_horizontal || options.horizontal_strength <= 0.0 {
        return s;
    }

    let t_min = curves.iter().map(|c| c.target(c.x_min).min(c.target(c.x_max))).fold(f64::MAX, f64::min);
    let t_max = curves.iter().map(|c| c.target(c.x_min).max(c.target(c.x_max))).fold(f64::MIN, f64::max);
    let j0 = ((t_min / GRID_CELL as f64) as i64).clamp(0, gh as i64 - 1) as usize;
    let j1 = ((t_max / GRID_CELL as f64).ceil() as i64).clamp(j0 as i64, gh as i64 - 1) as usize;

    let depth: Vec<f64> = (0..gw)
        .map(|i| {
            let sum: f64 = (j0..=j1).map(|j| (disp_grid[j * gw + i] as f64).abs()).sum();
            sum / (j1 - j0 + 1) as f64 * options.horizontal_strength
        })
        .collect();

    for i in 1..gw {
        let dz = depth[i] - depth[i - 1];
        s[i] = s[i - 1] + ((GRID_CELL * GRID_CELL) as f64 + dz * dz).sqrt();
    }
    s
}

fn invert_monotone(s: &[f64], v: f64) -> f64 {
    // s[i] = 弧長 (単調増加)、節点 i の x = i * GRID_CELL
    if v <= s[0] {
        return v;
    }
    let (mut lo, mut hi) = (0usize, s.len() - 1);
    if v >= s[hi] {
        return (hi * GRID_CELL) as f64 + (v - s[hi]);
    }
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if s[mid] <= v {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let f = (v - s[lo]) / (s[hi] - s[lo]);
    (lo as f64 + f) * GRID_CELL as f64
}

/// 解析結果を確認するためのデバッグ画像 (解析座標の縮小画像に、検出した曲線 (赤) と補正後の直線 (緑) を描く)。
pub fn debug_image(src: &RgbImage, options: &DewarpOptions) -> RgbImage {
    let gray = imgutil::to_gray(src);
    let (small, _) = imgutil::shrink_to_max_side(&gray, options.analysis_max_size);
    let mut out = image::DynamicImage::ImageLuma8(small.clone()).to_rgb8();
    let (mask, ch, n) = build_char_mask(&small);
    if n < 30 {
        return out;
    }
    let mut curves = detect_line_curves(&mask, ch, options);
    if curves.len() < options.min_lines {
        curves.extend(detect_envelope_curves(&mask, ch, options));
    }
    let (w, h) = out.dimensions();
    for c in &curves {
        let color = if c.is_envelope { Rgb([255, 0, 255]) } else { Rgb([255, 0, 0]) };
        let mut x = c.x_min;
        while x <= c.x_max {
            for (yy, col) in [(c.target(x), Rgb([0, 180, 0])), (c.y(x), color)] {
                let (xi, yi) = (x as i64, yy.round() as i64);
                if xi >= 0 && yi >= 0 && (xi as u32) < w && (yi as u32) < h {
                    out.put_pixel(xi as u32, yi as u32, col);
                }
            }
            x += 1.0;
        }
    }
    out
}

// ------------------------------------------------------------------
// 数値計算
// ------------------------------------------------------------------
fn poly_eval(c: &[f64], u: f64) -> f64 {
    c.iter().rev().fold(0.0, |r, &ci| r * u + ci)
}

fn poly_fit(us: &[f64], ys: &[f64], degree: usize) -> Option<Vec<f64>> {
    let n = degree + 1;
    let mut a = vec![vec![0.0; n + 1]; n];
    let mut p = vec![0.0; 2 * n];
    for (&u, &y) in us.iter().zip(ys) {
        let mut v = 1.0;
        for pi in p.iter_mut() {
            *pi = v;
            v *= u;
        }
        for i in 0..n {
            for j in 0..n {
                a[i][j] += p[i + j];
            }
            a[i][n] += p[i] * y;
        }
    }
    solve_linear(a, n)
}

fn solve_linear(mut a: Vec<Vec<f64>>, n: usize) -> Option<Vec<f64>> {
    for col in 0..n {
        let piv = (col..n).max_by(|&p, &q| a[p][col].abs().partial_cmp(&a[q][col].abs()).unwrap()).unwrap();
        if a[piv][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, piv);
        for r in 0..n {
            if r == col {
                continue;
            }
            let f = a[r][col] / a[col][col];
            if f == 0.0 {
                continue;
            }
            for c in col..=n {
                a[r][c] -= f * a[col][c];
            }
        }
    }
    Some((0..n).map(|i| a[i][n] / a[i][i]).collect())
}

fn weighted_line_fit(xs: &[f64], ys: &[f64], w: &[f64]) -> Option<(f64, f64)> {
    let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for i in 0..xs.len() {
        if w[i] <= 0.0 {
            continue;
        }
        sw += w[i];
        sx += w[i] * xs[i];
        sy += w[i] * ys[i];
        sxx += w[i] * xs[i] * xs[i];
        sxy += w[i] * xs[i] * ys[i];
    }
    let det = sw * sxx - sx * sx;
    if sw < 2.0 || det.abs() < 1e-9 {
        return None;
    }
    let b = (sw * sxy - sx * sy) / det;
    let a = (sy - b * sx) / sw;
    Some((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poly_fit_recovers_quadratic() {
        let us: Vec<f64> = (0..50).map(|i| i as f64 / 25.0 - 1.0).collect();
        let ys: Vec<f64> = us.iter().map(|u| 3.0 + 2.0 * u - 0.5 * u * u).collect();
        let p = poly_fit(&us, &ys, 2).unwrap();
        assert!((p[0] - 3.0).abs() < 1e-9 && (p[1] - 2.0).abs() < 1e-9 && (p[2] + 0.5).abs() < 1e-9);
    }

    #[test]
    fn blank_page_is_left_untouched() {
        let img = RgbImage::from_pixel(800, 1100, Rgb([250, 250, 250]));
        let (out, r) = dewarp(&img, &DewarpOptions::default());
        assert!(!r.applied);
        assert_eq!(out.dimensions(), img.dimensions());
    }
}
