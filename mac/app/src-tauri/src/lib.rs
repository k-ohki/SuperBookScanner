//! Tauri のバックエンド。画像処理はすべて book-core に任せ、ここでは画面とのやりとりだけを行う。

use anyhow::Result;
use base64::Engine;
use book_core::pipeline::{ConvertOptions, Progress};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

/// 画面で設定できる項目。ConvertOptions に変換して使う。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub rotate: u16,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderInfo {
    pub path: String,
    pub name: String,
    pub files: Vec<String>,
}

/// フォルダ内の画像をページ順に列挙する。
#[tauri::command]
fn open_folder(path: String) -> Result<FolderInfo, String> {
    let dir = PathBuf::from(&path);
    let files = book_core::input::list_images(&dir).map_err(|e| format!("{e:#}"))?;
    Ok(FolderInfo {
        name: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        path,
        files: files.iter().map(|f| f.display().to_string()).collect(),
    })
}

/// 元画像のサムネイル (JPEG の data URL)。
#[tauri::command]
async fn thumbnail(path: String, max_side: u32, rotate: u16) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<String> {
        let img = book_core::input::rotate_cw(book_core::input::load_page(Path::new(&path))?, rotate);
        let (w, h) = img.dimensions();
        let s = (max_side as f64 / w.max(h) as f64).min(1.0);
        // サムネイルは速さ優先 (image の thumbnail は単純な縮小で速い)
        let small = image::imageops::thumbnail(&img, ((w as f64 * s) as u32).max(1), ((h as f64 * s) as u32).max(1));
        data_url(&small)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("{e:#}"))
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
#[tauri::command]
async fn preview(path: String, settings: Settings, max_side: u32) -> Result<Vec<PreviewPage>, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<PreviewPage>> {
        let options = settings.to_options();
        book_core::pipeline::process_image(Path::new(&path), &options)?
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
            .collect()
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("{e:#}"))
}

#[derive(Default)]
struct ConvertState {
    cancel: Arc<AtomicBool>,
}

/// PDF に書き出す。進捗は "convert-progress" イベントで画面に送る。
#[tauri::command]
async fn convert(app: AppHandle, state: State<'_, ConvertState>, input: String, output: String, settings: Settings) -> Result<(), String> {
    let cancel = state.cancel.clone();
    cancel.store(false, Ordering::SeqCst);
    tauri::async_runtime::spawn_blocking(move || -> Result<()> {
        let options = settings.to_options();
        let progress = |p: Progress| {
            let _ = app.emit("convert-progress", p);
        };
        book_core::pipeline::convert_dir_cancellable(Path::new(&input), Path::new(&output), &options, &progress, &cancel)?;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn cancel_convert(state: State<'_, ConvertState>) {
    state.cancel.store(true, Ordering::SeqCst);
}

/// 起動時の引数でフォルダが渡されていれば、そのパス (`superbook-app <フォルダ>` で起動した場合)。
#[tauri::command]
fn initial_folder() -> Option<String> {
    std::env::args().skip(1).find(|a| Path::new(a).is_dir())
}

/// AI 鮮明化のプログラムが見つかるか (見つかればそのパス)。
#[tauri::command]
fn sharpen_available() -> Option<String> {
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(ConvertState::default())
        .invoke_handler(tauri::generate_handler![
            open_folder,
            thumbnail,
            preview,
            convert,
            cancel_convert,
            sharpen_available,
            initial_folder
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
