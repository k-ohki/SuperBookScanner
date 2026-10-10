//! 画面から呼ぶ処理の本体。Mac / Windows のアプリ (Tauri のコマンド) と、
//! iPad などから使う Web サーバー (remote) の両方から呼ぶので、Tauri には依存しない。
//! 重い処理 (thumbnail / preview / convert) はブロックするので、呼ぶ側で別スレッドに逃がす。

use anyhow::Result;
use base64::Engine;
use book_core::pipeline::{ConvertOptions, Progress};
use book_core::project::{PageOverride, Project};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// 画面で設定できる項目。ConvertOptions に変換して使う。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub rotate: u16,
    pub unwarp: bool,
    /// 平面化で紙の反りも直す (false なら台形だけ)
    #[serde(default = "yes")]
    pub unwarp_curl: bool,
    pub split: bool,
    pub deskew: bool,
    pub dewarp: bool,
    pub illumination: bool,
    pub crop: bool,
    pub margin: f64,
    pub quality: u8,
    pub rtl: bool,
    pub sharpen: bool,
    pub sharpen_scale: f64,
}

impl Settings {
    fn to_options(&self) -> ConvertOptions {
        let d = ConvertOptions::default();
        ConvertOptions {
            rotate: self.rotate,
            unwarp: self.unwarp.then(|| book_core::unwarp::UnwarpOptions {
                curl: self.unwarp_curl,
                ..d.unwarp.clone().unwrap_or_default()
            }),
            split: self.split.then(|| d.split.clone().unwrap_or_default()),
            deskew: self.deskew.then(|| d.deskew.clone().unwrap_or_default()),
            dewarp: self.dewarp.then(|| d.dewarp.clone().unwrap_or_default()),
            illumination: self.illumination.then(|| d.illumination.clone().unwrap_or_default()),
            layout: self.crop.then(|| book_core::layout::LayoutOptions {
                margin_ratio: self.margin,
                ..Default::default()
            }),
            sharpen: self.sharpen.then(|| book_core::sharpen::SharpenOptions {
                output_scale: self.sharpen_scale,
                ..Default::default()
            }),
            pdf: book_core::pdf::PdfOptions {
                jpeg_quality: self.quality,
                right_to_left: self.rtl,
                ..Default::default()
            },
            ..d
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderInfo {
    pub path: String,
    pub name: String,
    pub files: Vec<String>,
    /// ページごとの手動調整 (files と同じ順)
    pub overrides: Vec<PageOverride>,
}

/// フォルダ内の画像をページ順に列挙し、保存してある手動調整を読む。
pub fn open_folder(path: &str) -> Result<FolderInfo> {
    let dir = PathBuf::from(path);
    let files = book_core::input::list_images(&dir)?;
    let project = Project::load(&dir)?;
    Ok(FolderInfo {
        name: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        overrides: files.iter().map(|f| project.get(&dir, f)).collect(),
        files: files.iter().map(|f| f.display().to_string()).collect(),
        path: path.to_string(),
    })
}

/// 1 枚分の手動調整をフォルダの superbook.json に保存する (空なら消す)。
pub fn save_override(folder: &str, file: &str, value: PageOverride) -> Result<()> {
    let dir = PathBuf::from(folder);
    let mut project = Project::load(&dir)?;
    project.set(&dir, Path::new(file), value);
    project.save(&dir)
}

/// 元画像のサムネイル (JPEG の data URL)。
pub fn thumbnail(path: &str, max_side: u32, rotate: u16) -> Result<String> {
    let img = book_core::input::rotate_cw(book_core::input::load_page(Path::new(path))?, rotate);
    let (w, h) = img.dimensions();
    let s = (max_side as f64 / w.max(h) as f64).min(1.0);
    // サムネイルは速さ優先 (image の thumbnail は単純な縮小で速い)
    let small = image::imageops::thumbnail(&img, ((w as f64 * s) as u32).max(1), ((h as f64 * s) as u32).max(1));
    data_url(&small)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    /// 回転・平面化の後、分割の前の画像 (分割線を動かす画面用)
    pub stage: String,
    pub stage_width: u32,
    pub stage_height: u32,
    pub pages: Vec<PreviewPage>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPage {
    /// 補正後の画像 (JPEG の data URL)
    pub image: String,
    pub width: u32,
    pub height: u32,
    pub report: book_core::pipeline::PageReport,
}

/// 1 枚の入力画像を、書き出しと同じ設定で補正したプレビュー (見開きなら 2 ページ)。
pub fn preview(path: &str, settings: &Settings, value: &PageOverride, max_side: u32) -> Result<Preview> {
    let options = settings.to_options();
    let result = book_core::pipeline::process_image_with(Path::new(path), &options, value)?;
    let (stage_width, stage_height) = result.stage.dimensions();
    let stage = data_url(&shrink(&result.stage, max_side))?;
    let pages = result
        .pages
        .into_iter()
        .map(|(img, report)| {
            let (width, height) = img.dimensions();
            Ok(PreviewPage {
                image: data_url(&shrink(&img, max_side))?,
                width,
                height,
                report,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Preview {
        stage,
        stage_width,
        stage_height,
        pages,
    })
}

/// フォルダの画像を PDF に書き出す。
pub fn convert(input: &str, output: &str, settings: &Settings, progress: &(dyn Fn(Progress) + Sync), cancel: &AtomicBool) -> Result<()> {
    book_core::pipeline::convert_dir_cancellable(Path::new(input), Path::new(output), &settings.to_options(), progress, cancel)?;
    Ok(())
}

/// AI 鮮明化のプログラムが見つかるか (見つかればそのパス)。
pub fn sharpen_available() -> Option<String> {
    book_core::sharpen::find_binary().map(|p| p.display().to_string())
}

fn shrink(img: &image::RgbImage, max_side: u32) -> image::RgbImage {
    let (w, h) = img.dimensions();
    let long = w.max(h);
    if long <= max_side {
        return img.clone();
    }
    let s = max_side as f64 / long as f64;
    image::imageops::resize(
        img,
        ((w as f64 * s) as u32).max(1),
        ((h as f64 * s) as u32).max(1),
        image::imageops::FilterType::Triangle,
    )
}

fn data_url(img: &image::RgbImage) -> Result<String> {
    let mut buf = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 82).encode_image(img)?;
    Ok(format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(buf)))
}
