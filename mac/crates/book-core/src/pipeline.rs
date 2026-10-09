//! 画像フォルダ 1 つ (= 1 冊) を PDF にするパイプライン。
//!
//! 1. 入力 (自然順・EXIF 回転・透過を白に・A4 300dpi 相当の大きさに揃える)
//! 2. 傾き補正
//! 3. 歪み補正 (ノドの湾曲)
//! 4. 影・照明ムラの除去
//! 5. 余白の統一 (本文の外側を白で埋め、全ページを同じ大きさに切り出す)
//! 6. PDF 出力
//!
//! 1〜4 はページごとに並列で処理し、中間画像は作業フォルダに PNG で保存する (全ページをメモリに載せないため)。

use crate::deskew::{self, DeskewOptions, DeskewResult};
use crate::dewarp::{self, DewarpOptions, DewarpResult};
use crate::illumination::{self, IlluminationOptions};
use crate::input;
use crate::layout::{self, LayoutOptions, Rect};
use crate::pdf::{self, PdfOptions};
use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ConvertOptions {
    /// 入力画像を収める枠 (px)。既定は A4 300dpi
    pub page_box: (u32, u32),
    pub deskew: Option<DeskewOptions>,
    pub dewarp: Option<DewarpOptions>,
    pub illumination: Option<IlluminationOptions>,
    pub layout: Option<LayoutOptions>,
    pub pdf: PdfOptions,
    /// 先頭から何ページだけ処理するか (動作確認用)
    pub max_pages: Option<usize>,
    /// 中間画像を残すフォルダ (指定しなければ一時フォルダを使い、終了時に消す)
    pub work_dir: Option<PathBuf>,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            page_box: (2480, 3508),
            deskew: Some(DeskewOptions::default()),
            dewarp: Some(DewarpOptions::default()),
            illumination: Some(IlluminationOptions::default()),
            layout: Some(LayoutOptions::default()),
            pdf: PdfOptions::default(),
            max_pages: None,
            work_dir: None,
        }
    }
}

/// 進捗の通知 (UI やコマンドラインの表示用)。
#[derive(Clone, Debug, Serialize)]
pub enum Progress {
    /// ページの補正 (1〜4) が 1 ページ終わった
    PageProcessed {
        done: usize,
        total: usize,
        file: String,
    },
    /// PDF 用の切り出しとエンコードが 1 ページ終わった
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
    pub deskew: Option<DeskewResult>,
    pub dewarp: Option<DewarpResult>,
    pub content_box: Option<Rect>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConvertReport {
    pub pages: Vec<PageReport>,
    pub output_size_px: (u32, u32),
}

/// 画像フォルダを PDF に変換する。
pub fn convert_dir(input_dir: &Path, output_pdf: &Path, options: &ConvertOptions, progress: &(dyn Fn(Progress) + Sync)) -> Result<ConvertReport> {
    let mut files = input::list_images(input_dir)?;
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

    let result = convert_files(&files, &work_dir, output_pdf, options, progress);

    if temporary {
        let _ = std::fs::remove_dir_all(&work_dir);
    }
    result
}

fn convert_files(
    files: &[PathBuf],
    work_dir: &Path,
    output_pdf: &Path,
    options: &ConvertOptions,
    progress: &(dyn Fn(Progress) + Sync),
) -> Result<ConvertReport> {
    let total = files.len();
    let done = AtomicUsize::new(0);

    // ---- 1〜4: ページごとの補正 ----
    let pages: Vec<(PageReport, PathBuf, (u32, u32))> = files
        .par_iter()
        .enumerate()
        .map(|(i, file)| -> Result<_> {
            let mut report = PageReport {
                source: file.clone(),
                ..Default::default()
            };

            let img = input::load_page(file)?;
            let mut img = input::fit_to_box(&img, options.page_box.0, options.page_box.1);

            if let Some(o) = &options.deskew {
                let (out, r) = deskew::deskew(&img, o);
                img = out;
                report.deskew = Some(r);
            }
            if let Some(o) = &options.dewarp {
                let (out, r) = dewarp::dewarp(&img, o);
                img = out;
                report.dewarp = Some(r);
            }
            if let Some(o) = &options.illumination {
                img = illumination::normalize_illumination(&img, o);
            }
            if let Some(o) = &options.layout {
                report.content_box = layout::content_box(&img, o);
            }

            let path = work_dir.join(format!("page_{i:05}.png"));
            save_png_fast(&img, &path)?;

            let n = done.fetch_add(1, Ordering::SeqCst) + 1;
            progress(Progress::PageProcessed {
                done: n,
                total,
                file: file.display().to_string(),
            });
            Ok((report, path, img.dimensions()))
        })
        .collect::<Result<_>>()?;

    // ---- 5: 余白の統一 ----
    let (out_size, origins) = match &options.layout {
        Some(o) => {
            let input: Vec<_> = pages.iter().map(|(r, _, (w, h))| (*w, *h, r.content_box)).collect();
            let (size, origins) = layout::plan_crops(&input, o);
            (Some(size), origins)
        }
        None => (None, vec![(0, 0); pages.len()]),
    };

    // ---- 6: PDF ----
    let encoded_count = AtomicUsize::new(0);
    let encoded: Vec<pdf::EncodedPage> = pages
        .par_iter()
        .zip(origins.par_iter())
        .map(|((report, path, _), &(ox, oy))| -> Result<_> {
            let mut img = image::open(path)?.to_rgb8();
            let img = match out_size {
                Some((w, h)) => {
                    if let Some(b) = report.content_box {
                        let pad = (b.w.min(b.h) / 100).max(4);
                        layout::fill_outside(&mut img, b, pad);
                    }
                    layout::crop_with_padding(&img, ox, oy, w, h)
                }
                None => img,
            };
            let page = pdf::encode_page(&img, &options.pdf)?;
            let n = encoded_count.fetch_add(1, Ordering::SeqCst) + 1;
            progress(Progress::PageEncoded { done: n, total });
            Ok(page)
        })
        .collect::<Result<_>>()?;

    if let Some(parent) = output_pdf.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    pdf::write_pdf(&encoded, output_pdf, &options.pdf)?;
    progress(Progress::Finished {
        output: output_pdf.to_path_buf(),
    });

    let output_size_px = out_size.unwrap_or_else(|| pages.first().map(|p| p.2).unwrap_or_default());
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
