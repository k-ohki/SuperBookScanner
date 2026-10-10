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
//!      文字の行のほかに、枠・写真・帯の縁など「本来まっすぐな横の境目」も同じように使う
//!      (文章がない上の方や、図の多いページでも曲がりを直せるように)
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
    /// 枠・写真・帯の縁などの長い横の境目も、まっすぐにする手がかりに使う
    pub use_edges: bool,
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
            use_edges: true,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DewarpResult {
    pub applied: bool,
    pub message: String,
    pub num_line_curves: usize,
    pub num_envelope_curves: usize,
    #[serde(default)]
    pub num_edge_curves: usize,
    #[serde(default)]
    pub num_chain_curves: usize,
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
    is_edge: bool,
    is_chain: bool,
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

    let chains = detect_char_chains(&char_mask, char_height, line_pitch(&curves, char_height), options);
    merge_chains(&mut curves, chains, char_height);
    result.num_chain_curves = curves.iter().filter(|c| c.is_chain).count();

    if options.use_edges {
        let edges = detect_edge_curves(&edge_source(src, small.dimensions()), char_height, options);
        result.num_edge_curves = edges.len();
        curves.extend(edges);
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
    // 窓は x の距離で決める (文字ごとの点のようにまばらでも同じ幅になるように)
    let half = char_height * 2.0;
    let step = (char_height / 2.0).max(1.0);
    let (x0, x1) = (xs[0], xs[xs.len() - 1]);
    let n = ((x1 - x0) / step).floor() as usize + 1;
    let mut detail: Vec<f64> = Vec::with_capacity(n);
    let (mut lo, mut hi) = (0, 0);
    let mut window: Vec<f64> = Vec::new();
    for i in 0..n {
        let x = x0 + i as f64 * step;
        while lo < xs.len() && xs[lo] < x - half {
            lo += 1;
        }
        while hi < xs.len() && xs[hi] <= x + half {
            hi += 1;
        }
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

/// 同じ高さに並ぶ文字を、少し間が空いていても左から右へつないで「行」にする。
/// 途中で段になってずれた行 (横につながった塊としては 2 つに分かれてしまう) も、1 本の行として扱えるようにする。
fn detect_char_chains(char_mask: &GrayImage, char_height: f64, line_pitch: Option<f64>, options: &DewarpOptions) -> Vec<Curve> {
    let w = char_mask.width() as usize;
    // 1 文字が上下の部品 (へん・つくり・濁点など) に分かれていると並びが乱れるので、縦に少し閉じて 1 つにする
    let r = ((char_height * 0.25).round() as u32).max(1);
    let merged = erode_vertical(&dilate_vertical(char_mask, r), r);
    let (_, stats) = imgutil::components_with_stats(&merged);
    // ルビや点などの小さなものと、図の一部などの大きなものは除く
    let mut chars: Vec<(f64, f64)> = stats
        .iter()
        .skip(1)
        .filter(|st| {
            let (cw, ch) = (st.width as f64, st.height as f64);
            st.area > 0 && ch >= char_height * 0.5 && ch <= char_height * 1.6 && cw <= char_height * 2.0
        })
        .map(|st| (st.left as f64 + st.width as f64 / 2.0, st.top as f64 + st.height as f64 / 2.0))
        .collect();
    chars.sort_by(|a, b| a.0.total_cmp(&b.0));
    let n = chars.len();
    let max_dx = char_height * 8.0;
    // 縦のずれは、行の間隔の半分未満なら同じ行とみなす (隣の行とつながないように)
    let max_dy = line_pitch.map_or(char_height * 0.8, |p| (p * 0.45).clamp(char_height * 0.5, char_height * 1.2));

    // つなぐ候補 (左の文字, 右の文字, 費用) を費用の小さい順に、1 対 1 で採用する
    let mut links: Vec<(usize, usize, f64)> = Vec::new();
    for i in 0..n {
        for j in i + 1..n {
            let dx = chars[j].0 - chars[i].0;
            if dx > max_dx {
                break;
            }
            let dy = (chars[j].1 - chars[i].1).abs();
            if dx > char_height * 0.3 && dy <= max_dy {
                links.push((i, j, dx + 4.0 * dy));
            }
        }
    }
    links.sort_by(|a, b| a.2.total_cmp(&b.2));
    let mut next = vec![usize::MAX; n];
    let mut has_prev = vec![false; n];
    for &(i, j, _) in &links {
        if next[i] == usize::MAX && !has_prev[j] {
            next[i] = j;
            has_prev[j] = true;
        }
    }

    let cx = w as f64 / 2.0;
    let mut curves = Vec::new();
    for start in (0..n).filter(|&i| !has_prev[i]) {
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        let mut i = start;
        loop {
            xs.push(chars[i].0);
            ys.push(chars[i].1);
            if next[i] == usize::MAX {
                break;
            }
            i = next[i];
        }
        if xs.len() < 9 || xs[xs.len() - 1] - xs[0] < options.min_line_width_ratio * w as f64 {
            continue;
        }
        let span = (xs[xs.len() - 1] - xs[0]) / w as f64;
        let degree = if span < 0.35 { 2 } else { options.poly_degree };
        // 段差のある行は多項式から外れるので、許す誤差を広めにする (段差は add_detail で表す)
        if let Some(mut c) = fit_curve(&xs, &ys, degree, cx, char_height * 0.6, false, 0.0, 0.0) {
            add_detail(&mut c, &xs, &ys, char_height);
            c.is_chain = true;
            curves.push(c);
        }
    }
    curves
}

/// 枠・写真・色の帯の縁など、長くてまっすぐなはずの横の境目 (明るさが段になって変わるところ) を探す。
/// 細い罫線や地図の緯線 (両側が同じ明るさ) は、上下の範囲の中央値の差では反応しないので拾わない。
/// 曲がりくねった境目 (海岸線など) は、多項式との差が大きいので除く。
fn detect_edge_curves(gray: &GrayImage, char_height: f64, options: &DewarpOptions) -> Vec<Curve> {
    let (w, h) = (gray.width() as usize, gray.height() as usize);
    let k = ((char_height * 0.4).round() as usize).clamp(2, 64);
    if h < 4 * k + 2 {
        return Vec::new();
    }
    // 上 k 画素と下 k 画素の中央値の差 (中央値なので、k/2 より細い線には反応しない)
    let px = gray.as_raw();
    let mut g = vec![0f32; w * h];
    let median = |x: usize, from: usize, buf: &mut [u8; 64]| -> f32 {
        let b = &mut buf[..k];
        for (i, v) in b.iter_mut().enumerate() {
            *v = px[(from + i) * w + x];
        }
        b.sort_unstable();
        b[k / 2] as f32
    };
    // 位置は平均の差の山で決める (中央値の差は平らな山になり、位置がぼやけるため)
    let mut gm = vec![0f32; w * h];
    let mut buf = [0u8; 64];
    let mut col = vec![0u32; h + 1];
    for x in 0..w {
        for y in 0..h {
            col[y + 1] = col[y] + px[y * w + x] as u32;
        }
        for y in k..h - k {
            let up = median(x, y - k, &mut buf);
            let down = median(x, y, &mut buf);
            g[y * w + x] = down - up;
            gm[y * w + x] = (col[y + k] - col[y]) as f32 / k as f32 - (col[y] - col[y - k]) as f32 / k as f32;
        }
    }
    const THRESHOLD: f32 = 35.0;
    let cx = w as f64 / 2.0;
    let mut curves = Vec::new();
    // 明→暗 と 暗→明 は別々に扱う (帯の上下の縁がつながらないように)
    for sign in [1f32, -1f32] {
        let mut mask = GrayImage::new(w as u32, h as u32);
        for y in k + 1..h - k - 1 {
            for x in 0..w {
                let m = gm[y * w + x] * sign;
                // 中央値の差で境目かどうかを決め、平均の差が縦方向に極大のところだけを残す (細い線にする)
                if g[y * w + x] * sign > THRESHOLD && m >= gm[(y - 1) * w + x] * sign && m > gm[(y + 1) * w + x] * sign {
                    mask.put_pixel(x as u32, y as u32, image::Luma([255]));
                }
            }
        }
        // 段になってずれた境目 (折れ目) も 1 本につなぐため、縦に少し太らせてから連結成分を作る
        let reach = ((char_height * 0.3).round() as u32).max(1);
        let closed = dilate_vertical(&imgutil::close_horizontal(&mask, ((char_height * 1.3).round() as u32).max(3)), reach);
        let (labels, stats) = imgutil::components_with_stats(&closed);
        for (i, st) in stats.iter().enumerate().skip(1) {
            let (cw, chh) = (st.width as f64, st.height as f64 - 2.0 * reach as f64);
            if st.area == 0 || cw < options.min_line_width_ratio * w as f64 || chh > char_height * 0.8 + cw * 0.06 || cw < chh * 8.0 {
                continue;
            }
            let (x0, y0) = (st.left as usize, st.top as usize);
            let mut xs = Vec::new();
            let mut ys = Vec::new();
            for x in x0..x0 + st.width as usize {
                let (mut sum, mut n) = (0.0, 0.0);
                for y in y0..y0 + st.height as usize {
                    if labels[y * w + x] as usize == i && mask.get_pixel(x as u32, y as u32)[0] > 0 {
                        sum += y as f64;
                        n += 1.0;
                    }
                }
                if n > 0.0 {
                    xs.push(x as f64);
                    ys.push(sum / n);
                }
            }
            if (xs.len() as f64) < cw * 0.6 {
                continue;
            }
            let span = (xs[xs.len() - 1] - xs[0]) / w as f64;
            let degree = if span < 0.35 { 2 } else { options.poly_degree };
            if let Some(mut c) = fit_curve(&xs, &ys, degree, cx, char_height * 0.25, false, 0.0, 0.0) {
                add_detail(&mut c, &xs, &ys, char_height);
                c.is_edge = true;
                curves.push(c);
            }
        }
    }
    curves
}

/// 境目を探すための画像。画素ごとに R・G・B の最小値をとる (淡い色の帯や地図の枠も、白い紙との境目として見えるように)。
fn edge_source(src: &RgbImage, (w, h): (u32, u32)) -> GrayImage {
    let min_rgb = GrayImage::from_fn(src.width(), src.height(), |x, y| {
        let p = src.get_pixel(x, y);
        image::Luma([p[0].min(p[1]).min(p[2])])
    });
    image::imageops::resize(&min_rgb, w, h, image::imageops::FilterType::Triangle)
}

/// 文字の並び (chains) を、文字の塊からとった行 (curves) と合わせる。同じ行を両方がたどっているとき:
/// 塊の行が並びのほぼ全体を覆っていれば、位置が正確な塊の行を残す。
/// 並びの方が長い (段差で塊が途切れた行) なら、並びを残して、その中に収まる塊の行を捨てる。
fn merge_chains(curves: &mut Vec<Curve>, chains: Vec<Curve>, char_height: f64) {
    let same_row = |a: &Curve, b: &Curve| {
        let lo = a.x_min.max(b.x_min);
        let hi = a.x_max.min(b.x_max);
        hi > lo && (a.y((lo + hi) / 2.0) - b.y((lo + hi) / 2.0)).abs() < char_height * 0.6
    };
    for chain in chains {
        let len = chain.x_max - chain.x_min;
        let rows: Vec<usize> = (0..curves.len())
            .filter(|&i| !curves[i].is_edge && !curves[i].is_envelope && same_row(&curves[i], &chain))
            .collect();
        if rows
            .iter()
            .any(|&i| curves[i].x_max.min(chain.x_max) - curves[i].x_min.max(chain.x_min) >= len * 0.9)
        {
            continue;
        }
        // 並びの範囲に収まる (短い) 行を捨てて、並びを使う
        let mut k = 0;
        curves.retain(|c| {
            let drop = rows.contains(&k) && c.x_min >= chain.x_min - char_height && c.x_max <= chain.x_max + char_height;
            k += 1;
            !drop
        });
        curves.push(chain);
    }
}

/// 文字の行の間隔 (行の中央での位置の差の中央値)。行が少なければ None。
fn line_pitch(curves: &[Curve], char_height: f64) -> Option<f64> {
    let mut rows: Vec<(f64, f64, f64)> = curves
        .iter()
        .filter(|c| !c.is_envelope && !c.is_edge)
        .map(|c| {
            let mid = (c.x_min + c.x_max) / 2.0;
            (c.target(mid), c.x_min, c.x_max)
        })
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut gaps = Vec::new();
    for (i, a) in rows.iter().enumerate() {
        // 横に重なる、すぐ下の行との差
        if let Some(b) = rows[i + 1..].iter().find(|b| b.1 < a.2 && a.1 < b.2 && b.0 - a.0 > char_height * 0.8) {
            gaps.push(b.0 - a.0);
        }
    }
    if gaps.len() < 3 {
        return None;
    }
    gaps.sort_by(|a, b| a.total_cmp(b));
    Some(gaps[gaps.len() / 2])
}

/// 縦方向に r 画素ずつ細らせる (上下 r 画素がすべて 255 のところだけ残す)。
fn erode_vertical(mask: &GrayImage, r: u32) -> GrayImage {
    let (w, h) = mask.dimensions();
    GrayImage::from_fn(w, h, |x, y| {
        let lo = y.saturating_sub(r);
        let hi = (y + r).min(h - 1);
        let all = (lo..=hi).all(|yy| mask.get_pixel(x, yy)[0] > 0);
        image::Luma([if all { 255 } else { 0 }])
    })
}

/// 縦方向に r 画素ずつ太らせる。
fn dilate_vertical(mask: &GrayImage, r: u32) -> GrayImage {
    let (w, h) = mask.dimensions();
    let mut out = GrayImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            if mask.get_pixel(x, y)[0] > 0 {
                for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                    out.put_pixel(x, yy, image::Luma([255]));
                }
            }
        }
    }
    out
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
        is_edge: false,
        is_chain: false,
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
    let pitch = line_pitch(&curves, ch);
    let chains = detect_char_chains(&mask, ch, pitch, options);
    merge_chains(&mut curves, chains, ch);
    if options.use_edges {
        curves.extend(detect_edge_curves(&edge_source(src, small.dimensions()), ch, options));
    }
    let (w, h) = out.dimensions();
    for c in &curves {
        // 文字の行 = 赤、包絡線 = 紫、横の境目 = 青、文字の並び = 橙
        let color = if c.is_envelope {
            Rgb([255, 0, 255])
        } else if c.is_edge {
            Rgb([0, 90, 255])
        } else if c.is_chain {
            Rgb([255, 140, 0])
        } else {
            Rgb([255, 0, 0])
        };
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

    // 明るさが段になる横の境目 (帯の縁など) は、途中の段差も含めて拾う
    #[test]
    fn edge_with_step_is_detected() {
        let (w, h) = (1000u32, 400u32);
        // x < 500 では y = 200、x >= 500 では y = 190 で、上が白・下が灰色
        let img = GrayImage::from_fn(w, h, |x, y| {
            let edge = if x < 500 { 200 } else { 190 };
            image::Luma([if y < edge { 250 } else { 120 }])
        });
        let curves = detect_edge_curves(&img, 20.0, &DewarpOptions::default());
        assert_eq!(curves.len(), 1);
        let c = &curves[0];
        assert!(c.is_edge);
        // 段差の両側で、曲線が実際の境目の位置を通る
        assert!((c.y(250.0) - 200.0).abs() < 2.0, "{}", c.y(250.0));
        assert!((c.y(750.0) - 190.0).abs() < 2.0, "{}", c.y(750.0));
    }

    // 細い線 (罫線・地図の緯線) は、両側が同じ明るさなので拾わない
    #[test]
    fn thin_line_is_ignored() {
        let img = GrayImage::from_fn(1000, 400, |_, y| image::Luma([if (199..201).contains(&y) { 60 } else { 250 }]));
        assert!(detect_edge_curves(&img, 20.0, &DewarpOptions::default()).is_empty());
    }

    // 途中で段になってずれた行も、文字の並びとして 1 本につながる
    #[test]
    fn stepped_row_becomes_one_chain() {
        let mut mask = GrayImage::new(1000, 200);
        for k in 0..44 {
            let x0 = 20 + k * 22;
            let cy = if x0 < 500 { 100 } else { 88 };
            for y in cy - 8..cy + 8 {
                for x in x0..x0 + 14 {
                    mask.put_pixel(x, y, image::Luma([255]));
                }
            }
        }
        let chains = detect_char_chains(&mask, 16.0, None, &DewarpOptions::default());
        assert_eq!(chains.len(), 1);
        let c = &chains[0];
        assert!(c.x_min < 40.0 && c.x_max > 950.0);
        // 段差の両側の高さを表している
        assert!((c.y(200.0) - 100.0).abs() < 3.0, "{}", c.y(200.0));
        assert!((c.y(800.0) - 88.0).abs() < 3.0, "{}", c.y(800.0));
    }
}
