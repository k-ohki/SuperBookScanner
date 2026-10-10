//! 画像フォルダ 1 つ (= 1 冊) を PDF にするパイプライン。
//!
//! 1. 入力 (自然順・EXIF 回転・透過を白に)。見開き (横長) なら左右 2 ページに分け、A4 300dpi 相当の大きさに揃える
//! 2. 傾き補正
//! 3. 歪み補正 (ノドの湾曲)
//! 4. 影・照明ムラの除去
//! 5. 余白の統一 (本文の外側を白で埋め、全ページを同じ大きさに切り出す)
//! 6. AI 鮮明化 (任意。切り出した後のページを Real-ESRGAN で 4 倍にし、output_scale 倍に縮小する)
//! 7. PDF 出力
//!
//! 1〜4 はページごとに並列で処理し、中間画像は作業フォルダに PNG で保存する (全ページをメモリに載せないため)。

use crate::deskew::{self, DeskewOptions, DeskewResult};
use crate::dewarp::{self, DewarpOptions, DewarpResult};
use crate::illumination::{self, IlluminationOptions};
use crate::input;
use crate::layout::{self, LayoutOptions, Rect};
use crate::pdf::{self, PdfOptions};
use crate::project::{PageOverride, Project, SplitOverride};
use crate::sharpen::{self, SharpenOptions};
use crate::split::{self, SplitOptions, SplitResult};
use crate::unwarp::{self, UnwarpOptions, UnwarpResult};
use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ConvertOptions {
    /// 入力画像を収める枠 (px)。既定は A4 300dpi
    pub page_box: (u32, u32),
    /// 読み込み後に時計回りに回す角度 (0 / 90 / 180 / 270)。横向きに撮った写真用
    pub rotate: u16,
    /// 写真のページの平面化 (紙の範囲・台形・反り。UVDoc)。分割の前に見開きのまま行う
    pub unwarp: Option<UnwarpOptions>,
    /// 見開き分割
    pub split: Option<SplitOptions>,
    pub deskew: Option<DeskewOptions>,
    pub dewarp: Option<DewarpOptions>,
    pub illumination: Option<IlluminationOptions>,
    pub layout: Option<LayoutOptions>,
    /// AI 鮮明化 (None なら行わない)
    pub sharpen: Option<SharpenOptions>,
    pub pdf: PdfOptions,
    /// 先頭から何ページだけ処理するか (動作確認用)
    pub max_pages: Option<usize>,
    /// 中間画像を残すフォルダ (指定しなければ一時フォルダを使い、終了時に消す)
    pub work_dir: Option<PathBuf>,
    /// 入力フォルダの superbook.json (ページごとの手動調整) を使う
    pub use_project_file: bool,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            page_box: (2480, 3508),
            rotate: 0,
            unwarp: Some(UnwarpOptions::default()),
            split: Some(SplitOptions::default()),
            deskew: Some(DeskewOptions::default()),
            dewarp: Some(DewarpOptions::default()),
            illumination: Some(IlluminationOptions::default()),
            layout: Some(LayoutOptions::default()),
            sharpen: None,
            pdf: PdfOptions::default(),
            max_pages: None,
            work_dir: None,
            use_project_file: true,
        }
    }
}

/// 進捗の通知 (UI やコマンドラインの表示用)。
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind")]
pub enum Progress {
    /// ページの補正 (1〜4) が 1 ページ終わった
    PageProcessed {
        done: usize,
        total: usize,
        file: String,
    },
    /// AI 鮮明化が 1 ページ終わった
    PageSharpened {
        done: usize,
        total: usize,
    },
    /// PDF 用のエンコードが 1 ページ終わった
    PageEncoded {
        done: usize,
        total: usize,
    },
    Finished {
        output: PathBuf,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PageReport {
    pub source: PathBuf,
    pub unwarp: Option<UnwarpResult>,
    pub deskew: Option<DeskewResult>,
    pub dewarp: Option<DewarpResult>,
    pub content_box: Option<Rect>,
    /// 見開き分割の結果 (分割しなかった場合も記録する)
    pub split: SplitResult,
    /// 見開きを分割した場合、左 = 0 / 右 = 1
    pub half: Option<u8>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConvertReport {
    pub pages: Vec<PageReport>,
    pub output_size_px: (u32, u32),
}

/// 入力画像 1 枚分の補正結果。
pub struct ImageResult {
    /// 回転・平面化の後、分割の前の画像 (手動で分割位置を決める画面用)
    pub stage: image::RgbImage,
    /// 補正後のページ (見開きなら 2 ページ)
    pub pages: Vec<(image::RgbImage, PageReport)>,
}

/// 入力画像 1 枚分の補正を行う。見開きなら左右 2 ページに分けて、それぞれを補正する。
/// 処理: 読み込み → 回転 → 平面化 → 見開き分割 → 大きさの正規化 → 傾き補正 → 歪み補正 → 影の除去 → 本文の外接矩形。
/// 書き出し時と同じ処理なので、UI のプレビューにも使う。
pub fn process_image(file: &Path, options: &ConvertOptions) -> Result<Vec<(image::RgbImage, PageReport)>> {
    Ok(process_image_with(file, options, &PageOverride::default())?.pages)
}

/// `process_image` に、その画像の手動調整を反映した版。
pub fn process_image_with(file: &Path, options: &ConvertOptions, ovr: &PageOverride) -> Result<ImageResult> {
    let mut img = input::rotate_cw(input::load_page(file)?, ovr.rotate.unwrap_or(options.rotate));
    let unwarp_options = match ovr.unwarp {
        Some(false) => None,
        Some(true) => Some(options.unwarp.clone().unwrap_or_default()),
        None => options.unwarp.clone(),
    }
    .map(|mut o| {
        if let Some(curl) = ovr.unwarp_curl {
            o.curl = curl;
        }
        o
    });
    let photo = img.clone();
    let mut unwarp_grid = None;
    let unwarp_result = unwarp_options.as_ref().map(|o| {
        let (out, r, grid) = unwarp::unwarp_with_grid(&img, o);
        img = out;
        unwarp_grid = grid;
        r
    });

    let (mut halves, split_result) = match (ovr.split, &options.split) {
        (Some(SplitOverride::None), _) => (vec![img.clone()], SplitResult::default()),
        (Some(SplitOverride::At(r)), _) => split::split_at(&img, Some((r.clamp(0.0, 1.0) * img.width() as f64).round() as u32)),
        (None, Some(o)) => split::split_spread(&img, o),
        (None, None) => (vec![img.clone()], SplitResult::default()),
    };
    // 見開きなら、元の写真をノドで切って 1 ページずつ平面化し直す (UVDoc は 1 ページ用のモデルなので)
    let mut unwarp_results = vec![unwarp_result.clone(); halves.len()];
    if let (2, Some(grid), Some(gutter), Some(o)) = (halves.len(), &unwarp_grid, split_result.gutter_x, &unwarp_options) {
        let pages = unwarp::unwarp_pages(&photo, grid, gutter, o);
        if pages.iter().all(|(_, r)| r.applied) {
            unwarp_results = pages.iter().map(|(_, r)| Some(r.clone())).collect();
            halves = pages.into_iter().map(|(p, _)| p).collect();
        }
    }
    let n = halves.len();

    let pages = halves
        .into_iter()
        .enumerate()
        .map(|(i, half)| {
            let mut report = PageReport {
                source: file.to_path_buf(),
                unwarp: unwarp_results[i].clone(),
                split: split_result.clone(),
                half: if n > 1 { Some(i as u8) } else { None },
                ..Default::default()
            };
            let img = process_page_image(half, options, ovr, i as u8, &mut report);
            (img, report)
        })
        .collect();
    Ok(ImageResult { stage: img, pages })
}

// 1 ページ分 (分割後) の補正
fn process_page_image(img: image::RgbImage, options: &ConvertOptions, ovr: &PageOverride, half: u8, report: &mut PageReport) -> image::RgbImage {
    let mut img = input::fit_to_box(&img, options.page_box.0, options.page_box.1);

    if let Some(o) = &options.deskew {
        let (out, r) = deskew::deskew(&img, o);
        img = out;
        report.deskew = Some(r);
    }
    let dewarp_options = match ovr.dewarp {
        Some(false) => None,
        Some(true) => Some(options.dewarp.clone().unwrap_or_default()),
        None => options.dewarp.clone(),
    };
    if let Some(o) = &dewarp_options {
        let (out, r) = dewarp::dewarp(&img, o);
        img = out;
        report.dewarp = Some(r);
    }
    if let Some(o) = &options.illumination {
        img = illumination::normalize_illumination(&img, o);
    }
    if let Some(o) = &options.layout {
        report.content_box = match ovr.content.get(&half) {
            Some(r) => r.to_px(img.width(), img.height()),
            None => layout::content_box(&img, o),
        };
    }
    img
}

/// 画像フォルダを PDF に変換する。
pub fn convert_dir(input_dir: &Path, output_pdf: &Path, options: &ConvertOptions, progress: &(dyn Fn(Progress) + Sync)) -> Result<ConvertReport> {
    convert_dir_cancellable(input_dir, output_pdf, options, progress, &AtomicBool::new(false))
}

/// 中止されたときのエラーメッセージ。
pub const CANCELLED: &str = "cancelled";

/// `convert_dir` の中止できる版。`cancel` が true になると、できるだけ早く `CANCELLED` エラーで終わる。
pub fn convert_dir_cancellable(
    input_dir: &Path,
    output_pdf: &Path,
    options: &ConvertOptions,
    progress: &(dyn Fn(Progress) + Sync),
    cancel: &AtomicBool,
) -> Result<ConvertReport> {
    let project = if options.use_project_file {
        Project::load(input_dir)?
    } else {
        Project::default()
    };
    let files: Vec<(PathBuf, PageOverride)> = input::list_images(input_dir)?
        .into_iter()
        .map(|f| {
            let o = project.get(input_dir, &f);
            (f, o)
        })
        .filter(|(_, o)| !o.skip)
        .collect();
    let mut files = files;
    if let Some(n) = options.max_pages {
        files.truncate(n);
    }
    if files.is_empty() {
        bail!("no image files in '{}'", input_dir.display());
    }

    let (work_dir, temporary) = match &options.work_dir {
        Some(d) => (d.clone(), false),
        None => (std::env::temp_dir().join(format!("superbook-{}", std::process::id())), true),
    };
    std::fs::create_dir_all(&work_dir).with_context(|| format!("cannot create '{}'", work_dir.display()))?;

    let result = convert_files(&files, &work_dir, output_pdf, options, progress, cancel);

    if temporary {
        let _ = std::fs::remove_dir_all(&work_dir);
    }
    result
}

fn convert_files(
    files: &[(PathBuf, PageOverride)],
    work_dir: &Path,
    output_pdf: &Path,
    options: &ConvertOptions,
    progress: &(dyn Fn(Progress) + Sync),
    cancel: &AtomicBool,
) -> Result<ConvertReport> {
    let check_cancel = || -> Result<()> {
        if cancel.load(Ordering::SeqCst) {
            bail!(CANCELLED);
        }
        Ok(())
    };
    let total = files.len();
    let done = AtomicUsize::new(0);

    // ---- 1〜4: ページごとの補正 ----
    let pages: Vec<(PageReport, PathBuf, (u32, u32))> = files
        .par_iter()
        .enumerate()
        .map(|(i, (file, ovr))| -> Result<Vec<_>> {
            check_cancel()?;
            let results = process_image_with(file, options, ovr)?
                .pages
                .into_iter()
                .enumerate()
                .map(|(h, (img, report))| -> Result<_> {
                    let path = work_dir.join(format!("page_{i:05}_{h}.png"));
                    save_png_fast(&img, &path)?;
                    Ok((report, path, img.dimensions()))
                })
                .collect::<Result<Vec<_>>>()?;

            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            progress(Progress::PageProcessed {
                done: n,
                total,
                file: file.display().to_string(),
            });
            Ok(results)
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();
    // 見開きを分割した場合は、入力の枚数よりページ数が多くなる
    let total = pages.len();

    // ---- 5: 余白の統一 ----
    let (out_size, origins) = match &options.layout {
        Some(o) => {
            let input: Vec<_> = pages.iter().map(|(r, _, (w, h))| (*w, *h, r.content_box)).collect();
            let (size, origins) = layout::plan_crops(&input, o);
            (Some(size), origins)
        }
        None => (None, vec![(0, 0); pages.len()]),
    };

    // ---- 5 (続き): 切り出し ----
    let crop_page = |(report, path, _): &(PageReport, PathBuf, (u32, u32)), (ox, oy): (i64, i64)| -> Result<image::RgbImage> {
        check_cancel()?;
        let mut img = image::open(path)?.to_rgb8();
        Ok(match out_size {
            Some((w, h)) => {
                if let Some(b) = report.content_box {
                    let pad = (b.w.min(b.h) / 100).max(4);
                    layout::fill_outside(&mut img, b, pad);
                }
                layout::crop_with_padding(&img, ox, oy, w, h)
            }
            None => img,
        })
    };

    let encoded_count = AtomicUsize::new(0);
    let encode = |img: &image::RgbImage, pdf_options: &PdfOptions| -> Result<pdf::EncodedPage> {
        let page = pdf::encode_page(img, pdf_options)?;
        let n = encoded_count.fetch_add(1, Ordering::SeqCst) + 1;
        progress(Progress::PageEncoded { done: n, total });
        Ok(page)
    };

    let mut pdf_options = options.pdf.clone();
    let encoded: Vec<pdf::EncodedPage> = match &options.sharpen {
        None => pages
            .par_iter()
            .zip(origins.par_iter())
            .map(|(p, &o)| encode(&crop_page(p, o)?, &pdf_options))
            .collect::<Result<_>>()?,
        Some(sharpen_options) => {
            // ---- 6: AI 鮮明化 (切り出した後のページだけを処理する) ----
            let cropped_dir = work_dir.join("cropped");
            let sharp_dir = work_dir.join("sharpened");
            for d in [&cropped_dir, &sharp_dir] {
                if d.exists() {
                    std::fs::remove_dir_all(d)?;
                }
                std::fs::create_dir_all(d)?;
            }
            pages
                .par_iter()
                .zip(origins.par_iter())
                .enumerate()
                .try_for_each(|(i, (p, &o))| -> Result<()> { save_png_fast(&crop_page(p, o)?, &cropped_dir.join(format!("page_{i:05}.png"))) })?;

            sharpen::upscale_dir(
                &cropped_dir,
                &sharp_dir,
                sharpen_options,
                &|done| progress(Progress::PageSharpened { done, total }),
                cancel,
            )?;

            let scale = sharpen_options.output_scale.clamp(1.0, 4.0);
            pdf_options.dpi *= scale;
            (0..pages.len())
                .into_par_iter()
                .map(|i| -> Result<_> {
                    let name = format!("page_{i:05}.png");
                    let sharp = image::open(sharp_dir.join(&name))?.to_rgb8();
                    let base = image::image_dimensions(cropped_dir.join(&name))?;
                    let w = ((base.0 as f64 * scale).round() as u32).max(1);
                    let h = ((base.1 as f64 * scale).round() as u32).max(1);
                    let img = if (w, h) == sharp.dimensions() {
                        sharp
                    } else {
                        image::imageops::resize(&sharp, w, h, image::imageops::FilterType::Lanczos3)
                    };
                    encode(&img, &pdf_options)
                })
                .collect::<Result<_>>()?
        }
    };

    if let Some(parent) = output_pdf.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    pdf::write_pdf(&encoded, output_pdf, &pdf_options)?;
    progress(Progress::Finished {
        output: output_pdf.to_path_buf(),
    });

    let output_size_px = encoded.first().map(|p| (p.width, p.height)).unwrap_or_default();
    Ok(ConvertReport {
        pages: pages.into_iter().map(|p| p.0).collect(),
        output_size_px,
    })
}

/// 中間画像用。圧縮率より速度を優先して PNG で保存する。
pub fn save_png_fast(img: &image::RgbImage, path: &Path) -> Result<()> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let file = std::io::BufWriter::new(std::fs::File::create(path).with_context(|| format!("cannot create '{}'", path.display()))?);
    PngEncoder::new_with_quality(file, CompressionType::Fast, FilterType::Sub).write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(())
}
