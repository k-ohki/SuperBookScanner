//! AI 鮮明化。Real-ESRGAN の ncnn 版 (`realesrgan-ncnn-vulkan`) を外部プログラムとして呼び出す。
//! macOS 版は arm64 を含むユニバーサルバイナリで、MoltenVK 経由で Apple Silicon の GPU (Metal) で動く。
//! Windows 版は Vulkan に対応した GPU (NVIDIA / AMD / Intel) で動く。
//!
//! realesrgan-x4plus は 4 倍にしか拡大できないので、4 倍にしてから `output_scale` 倍 (既定 2 倍) に縮小する。
//! (C# 版の Real-ESRGAN 呼び出しの outscale = 2.0 と同じ出力解像度)

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const BINARY_NAME: &str = "realesrgan-ncnn-vulkan";

/// 実行ファイルのファイル名 (Windows では `.exe` が付く)。
pub fn binary_file_name() -> String {
    format!("{BINARY_NAME}{}", std::env::consts::EXE_SUFFIX)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct SharpenOptions {
    /// realesrgan-ncnn-vulkan の実行ファイル。None なら `find_binary` で探す
    pub binary: Option<PathBuf>,
    /// モデルのフォルダ。None なら実行ファイルと同じ場所の `models`
    pub models_dir: Option<PathBuf>,
    /// モデル名
    pub model: String,
    /// 元の画像に対する出力の倍率 (1.0〜4.0)
    pub output_scale: f64,
    /// タイルの大きさ (0 = 自動)。GPU メモリが少ない場合に小さくする
    pub tile_size: u32,
    /// GPU の番号 (None = 自動)
    pub gpu_id: Option<u32>,
}

impl Default for SharpenOptions {
    fn default() -> Self {
        Self {
            binary: None,
            models_dir: None,
            model: "realesrgan-x4plus".into(),
            output_scale: 2.0,
            tile_size: 0,
            gpu_id: None,
        }
    }
}

/// 実行ファイルを探す。順に: 環境変数 SUPERBOOK_REALESRGAN、自分の実行ファイルと同じフォルダ
/// (およびその下の `realesrgan/`)、.app の `Contents/Resources/realesrgan/`、リポジトリ直下の `third_party/realesrgan/` (開発時)、PATH。
pub fn find_binary() -> Option<PathBuf> {
    let name = binary_file_name();
    if let Some(p) = std::env::var_os("SUPERBOOK_REALESRGAN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
        if p.join(&name).is_file() {
            return Some(p.join(&name));
        }
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(&name));
            candidates.push(dir.join("realesrgan").join(&name));
            // macOS の .app: Contents/MacOS/<exe> → Contents/Resources/realesrgan/
            candidates.push(dir.join("../Resources/realesrgan").join(&name));
            // 開発時: target/release/superbook や
            // target/release/bundle/macos/SuperBookScanner.app/Contents/MacOS/ → third_party/realesrgan/
            // (Windows は target\release\superbook-windows.exe)
            for up in dir.ancestors().skip(1).take(7) {
                candidates.push(up.join("third_party").join("realesrgan").join(&name));
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|d| d.join(&name)));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// フォルダ内の PNG をすべて 4 倍に鮮明化して、out_dir に同じ名前の PNG で書き出す。
/// `on_file_done` は出力ファイルが 1 つ増えるたびに呼ばれる (進捗表示用)。
/// `cancel` が true になると、実行中のプロセスを止めてエラーを返す。
pub fn upscale_dir(in_dir: &Path, out_dir: &Path, options: &SharpenOptions, on_file_done: &(dyn Fn(usize) + Sync), cancel: &AtomicBool) -> Result<()> {
    let binary = match &options.binary {
        Some(b) => b.clone(),
        None => find_binary().with_context(|| {
            format!("'{BINARY_NAME}' not found. Run mac/scripts/fetch-realesrgan.sh (Windows: windows/scripts/fetch-realesrgan.ps1), or set SUPERBOOK_REALESRGAN to its path")
        })?,
    };
    let models = options
        .models_dir
        .clone()
        .unwrap_or_else(|| binary.parent().unwrap_or(Path::new(".")).join("models"));
    if !models.join(format!("{}.param", options.model)).is_file() {
        bail!("model '{}' not found in '{}'", options.model, models.display());
    }

    std::fs::create_dir_all(out_dir)?;
    let total = std::fs::read_dir(in_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
        .count();

    let mut cmd = crate::process::command(&binary);
    cmd.arg("-i")
        .arg(in_dir)
        .arg("-o")
        .arg(out_dir)
        .arg("-m")
        .arg(&models)
        .args(["-n", &options.model, "-s", "4", "-f", "png", "-t", &options.tile_size.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if let Some(g) = options.gpu_id {
        cmd.args(["-g", &g.to_string()]);
    }
    let mut child = cmd.spawn().with_context(|| format!("cannot start '{}'", binary.display()))?;

    // 標準エラー (進捗の % 表示) は読み捨てつつ、最後の数行をエラー表示用に残す
    let stderr = child.stderr.take().unwrap();
    let tail = std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let mut lines: Vec<String> = Vec::new();
        for line in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
            if !line.ends_with('%') {
                lines.push(line);
                if lines.len() > 20 {
                    lines.remove(0);
                }
            }
        }
        lines
    });

    // 出力フォルダのファイル数で進捗を通知する
    let finished = AtomicBool::new(false);
    let status = std::thread::scope(|s| {
        s.spawn(|| {
            let mut reported = 0;
            loop {
                let done = count_png(out_dir);
                while reported < done.min(total) {
                    reported += 1;
                    on_file_done(reported);
                }
                if finished.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        let status = loop {
            if cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
            }
            match child.try_wait() {
                Ok(Some(st)) => break Ok(st),
                Ok(None) => std::thread::sleep(Duration::from_millis(200)),
                Err(e) => break Err(e),
            }
        };
        finished.store(true, Ordering::SeqCst);
        status
    })?;

    if cancel.load(Ordering::SeqCst) {
        bail!(crate::pipeline::CANCELLED);
    }

    let log = tail.join().unwrap_or_default();
    if !status.success() || count_png(out_dir) < total {
        bail!("{BINARY_NAME} failed ({status}):\n{}", log.join("\n"));
    }
    Ok(())
}

fn count_png(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|r| r.filter_map(|e| e.ok()).filter(|e| e.path().extension().is_some_and(|x| x == "png")).count())
        .unwrap_or(0)
}
