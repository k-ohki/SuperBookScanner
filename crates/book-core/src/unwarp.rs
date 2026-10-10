//! 学習済みモデル UVDoc (MIT, https://github.com/tanguymagne/UVDoc) によるページの平面化。
//!
//! スマホで撮った写真から、紙の範囲・台形 (遠近)・紙の反りをまとめて補正し、机や指などの背景を落とす。
//! モデルは 488x712 に縮めた画像から、出力画像の各点が元画像のどこに当たるかを 45x31 の格子で返す。
//! その格子を元の解像度で補間して、元画像から画素を拾い直す (UVDoc の bilinear_unwarping と同じ)。
//! 推論は tract (Rust だけで動く ONNX ランタイム) で CPU 上で行う。
//!
//! UVDoc は 1 ページの文書で学習したモデルなので、見開きの写真をそのまま渡すと (横長の写真を縦長の入力に
//! 押し縮めることになり) ページの上の方などに折れたような段差を作ることがある。見開きは、全体を一度平面化して
//! ノドの位置を探し、元の写真をノドで左右に切ってから、1 ページずつ平面化し直す (`unwarp_pages`)。
//!
//! モデルはページの影を折り目や反りと見誤り、文字を曲げてしまうことがある。そのときのために、
//! 「紙の反りは直さない」(`curl = false`) では、格子の外周 (紙の輪郭) に最もよく合う射影変換 (台形補正) だけを使い、
//! 格子の内側の曲がりは捨てる。ノドの湾曲は、文字の行をたどる歪み補正 (dewarp) に任せる。

use crate::input;
use anyhow::{anyhow, Context, Result};
use image::{Rgb, RgbImage};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tract_onnx::prelude::*;

pub const MODEL_FILE: &str = "uvdoc.onnx";
const INPUT_W: usize = 488;
const INPUT_H: usize = 712;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct UnwarpOptions {
    /// モデルファイル。None なら find_model() で探す
    pub model: Option<PathBuf>,
    /// 紙の反り (曲面) も直す。false なら紙の範囲と台形 (遠近) だけを直す
    pub curl: bool,
}

impl Default for UnwarpOptions {
    fn default() -> Self {
        Self { model: None, curl: true }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UnwarpResult {
    pub applied: bool,
    pub message: String,
}

type Plan = Arc<TypedRunnableModel>;

/// モデルを探す。SUPERBOOK_UVDOC → 実行ファイルの隣 → .app の Resources → 開発時のリポジトリ直下の models/。
pub fn find_model() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SUPERBOOK_UVDOC") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
        if p.join(MODEL_FILE).is_file() {
            return Some(p.join(MODEL_FILE));
        }
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(MODEL_FILE));
            candidates.push(dir.join("models").join(MODEL_FILE));
            // Windows のインストール先: <exe> と同じフォルダの models/ (上の候補)
            // macOS の .app: Contents/MacOS/<exe> → Contents/Resources/models/
            candidates.push(dir.join("../Resources/models").join(MODEL_FILE));
            // 開発時: target/release/superbook → models/
            for up in dir.ancestors().skip(1).take(3) {
                candidates.push(up.join("models").join(MODEL_FILE));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// 読み込んだモデルはパスごとに使い回す (読み込みに 1 秒ほどかかるため)。
fn load_plan(path: &Path) -> Result<Plan> {
    static CACHE: OnceLock<Mutex<Vec<(PathBuf, Plan)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some((_, plan)) = cache.lock().unwrap().iter().find(|(p, _)| p == path) {
        return Ok(plan.clone());
    }
    let plan = tract_onnx::onnx()
        .model_for_path(path)
        .with_context(|| format!("{} を読めません", path.display()))?
        .with_input_fact(0, f32::fact([1, 3, INPUT_H, INPUT_W]).into())?
        .into_optimized()?
        .into_runnable()?;
    cache.lock().unwrap().push((path.to_path_buf(), plan.clone()));
    Ok(plan)
}

/// 格子 (2 x gh x gw、値は -1..1 の元画像座標) を推論する。
pub fn predict_grid(img: &RgbImage, model: &Path) -> Result<(Vec<f32>, usize, usize)> {
    let plan = load_plan(model)?;
    let small = image::imageops::resize(img, INPUT_W as u32, INPUT_H as u32, image::imageops::FilterType::Triangle);
    let input: Tensor =
        tract_ndarray::Array4::from_shape_fn((1, 3, INPUT_H, INPUT_W), |(_, c, y, x)| small.get_pixel(x as u32, y as u32)[c] as f32 / 255.0).into();
    let outputs = plan.run(tvec!(input.into()))?;
    let grid = outputs[0].to_plain_array_view::<f32>()?;
    let shape = grid.shape().to_vec();
    if shape.len() != 4 || shape[1] != 2 {
        return Err(anyhow!("想定外のモデル出力 {shape:?}"));
    }
    Ok((grid.iter().copied().collect(), shape[2], shape[3]))
}

/// 平面化に使った格子 (出力の各点が元画像のどこに当たるか。2 x gh x gw、-1..1)。
#[derive(Clone, Debug)]
pub struct UnwarpGrid {
    pub grid: Vec<f32>,
    pub gh: usize,
    pub gw: usize,
}

/// 写真のページを平面化する。出力は入力と同じ大きさ。
pub fn unwarp(img: &RgbImage, options: &UnwarpOptions) -> (RgbImage, UnwarpResult) {
    let (out, r, _) = unwarp_with_grid(img, options);
    (out, r)
}

/// `unwarp` と同じ。使った格子も返す (平面化できなかったときは None)。
pub fn unwarp_with_grid(img: &RgbImage, options: &UnwarpOptions) -> (RgbImage, UnwarpResult, Option<UnwarpGrid>) {
    let model = match options.model.clone().or_else(find_model) {
        Some(m) => m,
        None => {
            return (
                img.clone(),
                UnwarpResult {
                    applied: false,
                    message: format!("{MODEL_FILE} が見つかりません"),
                },
                None,
            )
        }
    };
    match predict_grid(img, &model) {
        Ok((grid, gh, gw)) => {
            let grid = if options.curl { grid } else { flatten_grid(&grid, gh, gw) };
            (
                remap(img, &grid, gh, gw),
                UnwarpResult {
                    applied: true,
                    message: String::new(),
                },
                Some(UnwarpGrid { grid, gh, gw }),
            )
        }
        Err(e) => (
            img.clone(),
            UnwarpResult {
                applied: false,
                message: format!("{e:#}"),
            },
            None,
        ),
    }
}

/// 見開きを 1 ページずつ平面化する。
/// `gutter_x` は、見開き全体を平面化した画像 (大きさは `src` と同じ) でのノドの位置。
/// それを格子で元の写真の位置に戻し、少し重なるように左右に切って、それぞれを平面化する。
pub fn unwarp_pages(src: &RgbImage, grid: &UnwarpGrid, gutter_x: u32, options: &UnwarpOptions) -> Vec<(RgbImage, UnwarpResult)> {
    let (w, h) = src.dimensions();
    let (left_end, right_start) = gutter_cut(w, grid, gutter_x);
    let left = image::imageops::crop_imm(src, 0, 0, left_end, h).to_image();
    let right = image::imageops::crop_imm(src, right_start, 0, w - right_start, h).to_image();
    [left, right].iter().map(|page| unwarp(page, options)).collect()
}

/// 元の写真を左右に切る位置 (左のページの右端, 右のページの左端)。ノドの線をはさんで少し重ねる。
/// 重ねすぎると隣のページが入って、ノド側が曲がって平面化される。
fn gutter_cut(w: u32, grid: &UnwarpGrid, gutter_x: u32) -> (u32, u32) {
    let (gw, gh) = (grid.gw, grid.gh);
    // ノドの列 (平面化後の x) に当たる、元の写真での x を上から下まで求める
    let gx = gutter_x as f64 / (w - 1).max(1) as f64 * (gw - 1) as f64;
    let xs: Vec<f64> = (0..gh)
        .map(|j| {
            let u = crate::imgutil::sample_bilinear_f32(&grid.grid[..gh * gw], gw, gh, gx, j as f64);
            (u + 1.0) / 2.0 * (w - 1) as f64
        })
        .collect();
    let lo = xs.iter().copied().fold(f64::MAX, f64::min);
    let hi = xs.iter().copied().fold(f64::MIN, f64::max);
    let margin = w as f64 * 0.01;
    let left_end = ((hi + margin).round() as u32).clamp(1, w);
    let right_start = ((lo - margin).round().max(0.0) as u32).min(w - 1);
    (left_end, right_start)
}

/// 格子を、その外周の点に最もよく合う射影変換 (ホモグラフィ) で作り直す。
/// 紙の範囲と台形 (遠近) は残し、内側の曲がり (反り・影による誤認識) をなくす。
fn flatten_grid(grid: &[f32], gh: usize, gw: usize) -> Vec<f32> {
    let plane = gh * gw;
    let st = |i: usize, j: usize| (i as f64 / (gw - 1) as f64, j as f64 / (gh - 1) as f64);
    let uv = |i: usize, j: usize| (grid[j * gw + i] as f64, grid[plane + j * gw + i] as f64);
    // 外周の格子点 (出力の正規化座標 s, t → 元画像の座標 u, v)
    let mut pts = Vec::new();
    for i in 0..gw {
        for j in [0, gh - 1] {
            pts.push((st(i, j), uv(i, j)));
        }
    }
    for j in 1..gh - 1 {
        for i in [0, gw - 1] {
            pts.push((st(i, j), uv(i, j)));
        }
    }
    let Some(h) = fit_homography(&pts) else {
        return grid.to_vec();
    };
    let mut out = vec![0f32; 2 * plane];
    for j in 0..gh {
        for i in 0..gw {
            let (s, t) = st(i, j);
            let d = h[6] * s + h[7] * t + 1.0;
            out[j * gw + i] = ((h[0] * s + h[1] * t + h[2]) / d) as f32;
            out[plane + j * gw + i] = ((h[3] * s + h[4] * t + h[5]) / d) as f32;
        }
    }
    out
}

/// (s, t) → (u, v) の射影変換を最小二乗で求める (h33 = 1 とした 8 個の係数)。
fn fit_homography(pts: &[((f64, f64), (f64, f64))]) -> Option<[f64; 8]> {
    // 正規方程式 A^T A h = A^T b
    let mut ata = [[0f64; 8]; 8];
    let mut atb = [0f64; 8];
    for &((s, t), (u, v)) in pts {
        for (row, rhs) in [([s, t, 1.0, 0.0, 0.0, 0.0, -u * s, -u * t], u), ([0.0, 0.0, 0.0, s, t, 1.0, -v * s, -v * t], v)] {
            for a in 0..8 {
                atb[a] += row[a] * rhs;
                for b in 0..8 {
                    ata[a][b] += row[a] * row[b];
                }
            }
        }
    }
    solve8(ata, atb)
}

/// 8 元連立一次方程式をガウスの消去法 (部分ピボット) で解く。
fn solve8(mut m: [[f64; 8]; 8], mut b: [f64; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        let pivot = (col..8).max_by(|&x, &y| m[x][col].abs().total_cmp(&m[y][col].abs()))?;
        if m[pivot][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, pivot);
        b.swap(col, pivot);
        for r in col + 1..8 {
            let f = m[r][col] / m[col][col];
            for c in col..8 {
                m[r][c] -= f * m[col][c];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = [0f64; 8];
    for r in (0..8).rev() {
        let sum: f64 = (r + 1..8).map(|c| m[r][c] * x[c]).sum();
        x[r] = (b[r] - sum) / m[r][r];
    }
    Some(x)
}

/// 格子を出力の大きさまで双線形で広げ、元画像から双線形で画素を拾う (align_corners = true)。
/// 元画像の外を指す点は白にする。
fn remap(img: &RgbImage, grid: &[f32], gh: usize, gw: usize) -> RgbImage {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let plane = gh * gw;
    let (gx, gy) = (&grid[..plane], &grid[plane..2 * plane]);
    let channels: Vec<Vec<f32>> = (0..3).map(|c| img.pixels().map(|p| p[c] as f32).collect()).collect();
    let sx = (gw - 1) as f64 / (w - 1).max(1) as f64;
    let sy = (gh - 1) as f64 / (h - 1).max(1) as f64;

    let mut out = RgbImage::new(w as u32, h as u32);
    out.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
        let gyf = y as f64 * sy;
        for x in 0..w {
            let gxf = x as f64 * sx;
            let u = crate::imgutil::sample_bilinear_f32(gx, gw, gh, gxf, gyf);
            let v = crate::imgutil::sample_bilinear_f32(gy, gw, gh, gxf, gyf);
            let px = (u + 1.0) / 2.0 * (w - 1) as f64;
            let py = (v + 1.0) / 2.0 * (h - 1) as f64;
            let pixel = if px < -0.5 || py < -0.5 || px > w as f64 - 0.5 || py > h as f64 - 0.5 {
                Rgb([255, 255, 255])
            } else {
                Rgb([0, 1, 2].map(|c| crate::imgutil::sample_bilinear_f32(&channels[c], w, h, px, py).round().clamp(0.0, 255.0) as u8))
            };
            row[x * 3..x * 3 + 3].copy_from_slice(&pixel.0);
        }
    });
    out
}

/// 動作確認用: ファイルを読み、平面化して保存する。
pub fn unwarp_file(src: &Path, dst: &Path, rotate: u16, options: &UnwarpOptions) -> Result<UnwarpResult> {
    let img = input::rotate_cw(input::load_page(src)?, rotate);
    let (out, r) = unwarp(&img, options);
    out.save(dst)?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid_from(gh: usize, gw: usize, f: impl Fn(f64, f64) -> (f64, f64)) -> Vec<f32> {
        let mut grid = vec![0f32; 2 * gh * gw];
        for j in 0..gh {
            for i in 0..gw {
                let (u, v) = f(i as f64 / (gw - 1) as f64, j as f64 / (gh - 1) as f64);
                grid[j * gw + i] = u as f32;
                grid[gh * gw + j * gw + i] = v as f32;
            }
        }
        grid
    }

    // 射影変換だけの格子は、平らにしてもほぼ変わらない
    #[test]
    fn flatten_keeps_perspective() {
        let persp = |s: f64, t: f64| {
            let d = 0.3 * s + 0.1 * t + 1.0;
            ((1.6 * s - 0.2 * t - 0.8) / d, (0.1 * s + 1.5 * t - 0.9) / d)
        };
        let grid = grid_from(45, 31, persp);
        let flat = flatten_grid(&grid, 45, 31);
        let err = grid.iter().zip(&flat).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(err < 1e-4, "max error {err}");
    }

    // 内側だけが曲がった格子 (影を折り目と見誤った場合) は、まっすぐに戻る
    #[test]
    fn flatten_removes_inner_bend() {
        let straight = |s: f64, t: f64| (s * 1.8 - 0.9, t * 1.8 - 0.9);
        let bent = |s: f64, t: f64| {
            let (u, v) = straight(s, t);
            // 外周では 0、内側で最大になる曲がり
            (u, v + 0.08 * (std::f64::consts::PI * s).sin() * (std::f64::consts::PI * t).sin())
        };
        let flat = flatten_grid(&grid_from(45, 31, bent), 45, 31);
        let want = grid_from(45, 31, straight);
        let err = want.iter().zip(&flat).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
        assert!(err < 1e-4, "max error {err}");
    }

    // 恒等の格子なら画像はそのまま
    #[test]
    fn identity_grid_keeps_image() {
        let img = RgbImage::from_fn(40, 30, |x, y| Rgb([(x * 6) as u8, (y * 8) as u8, 100]));
        let (gh, gw) = (5, 4);
        let mut grid = vec![0f32; 2 * gh * gw];
        for j in 0..gh {
            for i in 0..gw {
                grid[j * gw + i] = i as f32 / (gw - 1) as f32 * 2.0 - 1.0;
                grid[gh * gw + j * gw + i] = j as f32 / (gh - 1) as f32 * 2.0 - 1.0;
            }
        }
        let out = remap(&img, &grid, gh, gw);
        for (a, b) in img.pixels().zip(out.pixels()) {
            for c in 0..3 {
                assert!((a[c] as i32 - b[c] as i32).abs() <= 1);
            }
        }
    }

    // ノドが写真の中で斜めでも、その線をはさむように切る
    #[test]
    fn gutter_cut_follows_slanted_gutter() {
        // 出力の x = 中央の列が、元の写真では上で 0.1、下で -0.1 (正規化座標) に当たる
        let (gh, gw) = (5, 5);
        let grid = grid_from(gh, gw, |s, t| (s * 2.0 - 1.0 + (0.1 - 0.2 * t) * (1.0 - (2.0 * s - 1.0).abs()), t * 2.0 - 1.0));
        let w = 1001;
        let (left_end, right_start) = gutter_cut(w, &UnwarpGrid { grid, gh, gw }, 500);
        // 上端は x = 550、下端は x = 450。それぞれ 1% (10 px) 重ねる
        assert!((left_end as i32 - 560).abs() <= 2, "{left_end}");
        assert!((right_start as i32 - 440).abs() <= 2, "{right_start}");
    }
}
