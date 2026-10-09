//! 学習済みモデル UVDoc (MIT, https://github.com/tanguymagne/UVDoc) によるページの平面化。
//!
//! スマホで撮った写真から、紙の範囲・台形 (遠近)・紙の反りをまとめて補正し、机や指などの背景を落とす。
//! モデルは 488x712 に縮めた画像から、出力画像の各点が元画像のどこに当たるかを 45x31 の格子で返す。
//! その格子を元の解像度で補間して、元画像から画素を拾い直す (UVDoc の bilinear_unwarping と同じ)。
//! 推論は tract (Rust だけで動く ONNX ランタイム) で CPU 上で行う。

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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UnwarpOptions {
    /// モデルファイル。None なら find_model() で探す
    pub model: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UnwarpResult {
    pub applied: bool,
    pub message: String,
}

type Plan = Arc<TypedRunnableModel>;

/// モデルを探す。SUPERBOOK_UVDOC → 実行ファイルの隣 → .app の Resources → 開発時の mac/models/。
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
            // macOS の .app: Contents/MacOS/<exe> → Contents/Resources/models/
            candidates.push(dir.join("../Resources/models").join(MODEL_FILE));
            // 開発時: mac/target/release/superbook → mac/models/
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

/// 写真のページを平面化する。出力は入力と同じ大きさ。
pub fn unwarp(img: &RgbImage, options: &UnwarpOptions) -> (RgbImage, UnwarpResult) {
    let model = match options.model.clone().or_else(find_model) {
        Some(m) => m,
        None => {
            return (
                img.clone(),
                UnwarpResult {
                    applied: false,
                    message: format!("{MODEL_FILE} が見つかりません"),
                },
            )
        }
    };
    match predict_grid(img, &model) {
        Ok((grid, gh, gw)) => (
            remap(img, &grid, gh, gw),
            UnwarpResult {
                applied: true,
                message: String::new(),
            },
        ),
        Err(e) => (
            img.clone(),
            UnwarpResult {
                applied: false,
                message: format!("{e:#}"),
            },
        ),
    }
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
}
