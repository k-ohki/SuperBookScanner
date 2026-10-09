//! 本のページ画像 (スキャン・スマホ撮影) から、読みやすい PDF を作る画像処理ライブラリ。
//! UI に依存しないので、CLI (book-cli) からも Tauri アプリからも使う。

pub mod deskew;
pub mod dewarp;
pub mod illumination;
pub mod imgutil;
pub mod input;
pub mod layout;
pub mod pdf;
pub mod pipeline;
pub mod sharpen;

pub use pipeline::{convert_dir, ConvertOptions, ConvertReport, Progress};
