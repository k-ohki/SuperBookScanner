//! superbook: 本のページ画像のフォルダを、読みやすい PDF にするコマンド。

use anyhow::{bail, Result};
use book_core::pipeline::{convert_dir, ConvertOptions, Progress};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "superbook", version, about = "Turn photos/scans of book pages into a clean PDF")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 画像フォルダ (1 フォルダ = 1 冊) を PDF にする
    Convert {
        /// 入力フォルダ。--recursive のときは、画像を含むサブフォルダをそれぞれ 1 冊として扱う
        input: PathBuf,
        /// 出力 PDF (--recursive のときは出力フォルダ)
        #[arg(short, long)]
        output: PathBuf,
        /// サブフォルダをまとめて変換する
        #[arg(short, long)]
        recursive: bool,
        /// 傾き補正をしない
        #[arg(long)]
        no_deskew: bool,
        /// 歪み (ノドの湾曲) 補正をしない
        #[arg(long)]
        no_dewarp: bool,
        /// 影・照明ムラの除去をしない
        #[arg(long)]
        no_illumination: bool,
        /// 余白の統一 (切り出し) をしない
        #[arg(long)]
        no_crop: bool,
        /// 余白 (本文の短辺に対する割合)
        #[arg(long, default_value_t = 0.05)]
        margin: f64,
        /// JPEG の品質 (1-100)
        #[arg(long, default_value_t = 85)]
        quality: u8,
        /// 右綴じ (縦書きの本)
        #[arg(long)]
        rtl: bool,
        /// 入力画像を揃える長辺のピクセル数 (既定 3508 = A4 300dpi)
        #[arg(long, default_value_t = 3508)]
        page_long_side: u32,
        /// AI 鮮明化 (Real-ESRGAN) を行う
        #[arg(long)]
        sharpen: bool,
        /// 鮮明化後の解像度の倍率 (1〜4。2 なら 600dpi 相当)
        #[arg(long, default_value_t = 2.0)]
        sharpen_scale: f64,
        /// 鮮明化のモデル (realesrgan-x4plus / realesrnet-x4plus など)
        #[arg(long, default_value = "realesrgan-x4plus")]
        sharpen_model: String,
        /// realesrgan-ncnn-vulkan の実行ファイル (省略時は自動で探す)
        #[arg(long)]
        realesrgan: Option<PathBuf>,
        /// 先頭から N ページだけ処理する (動作確認用)
        #[arg(long)]
        max_pages: Option<usize>,
        /// 中間画像と処理結果 (report.json) を残すフォルダ
        #[arg(long)]
        work_dir: Option<PathBuf>,
    },
    /// 1 枚の画像の歪み補正だけを行う (動作確認用)。--debug で検出した行の画像も出す
    Dewarp {
        input: PathBuf,
        output: PathBuf,
        #[arg(long)]
        debug: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Convert {
            input,
            output,
            recursive,
            no_deskew,
            no_dewarp,
            no_illumination,
            no_crop,
            margin,
            quality,
            rtl,
            page_long_side,
            sharpen,
            sharpen_scale,
            sharpen_model,
            realesrgan,
            max_pages,
            work_dir,
        } => {
            let mut opts = ConvertOptions::default();
            // A4 の縦横比 (2480 x 3508) のまま長辺を合わせる
            opts.page_box = (((page_long_side as f64) * 2480.0 / 3508.0).round() as u32, page_long_side);
            opts.pdf.dpi = 300.0 * page_long_side as f64 / 3508.0;
            if no_deskew {
                opts.deskew = None;
            }
            if no_dewarp {
                opts.dewarp = None;
            }
            if no_illumination {
                opts.illumination = None;
            }
            if no_crop {
                opts.layout = None;
            } else if let Some(l) = opts.layout.as_mut() {
                l.margin_ratio = margin;
            }
            opts.pdf.jpeg_quality = quality;
            opts.pdf.right_to_left = rtl;
            if sharpen && page_long_side < 2400 {
                eprintln!("warning: --sharpen on low-resolution pages (--page-long-side {page_long_side}) can turn small letters into wrong shapes");
            }
            if sharpen {
                opts.sharpen = Some(book_core::sharpen::SharpenOptions {
                    binary: realesrgan,
                    model: sharpen_model,
                    output_scale: sharpen_scale,
                    ..Default::default()
                });
            }
            opts.max_pages = max_pages;
            opts.work_dir = work_dir;

            if recursive {
                convert_tree(&input, &output, &opts)
            } else {
                convert_one(&input, &output, &opts)
            }
        }
        Command::Dewarp { input, output, debug } => {
            let img = book_core::input::load_page(&input)?;
            let opts = book_core::dewarp::DewarpOptions::default();
            let (out, r) = book_core::dewarp::dewarp(&img, &opts);
            println!("{}", serde_json_string(&r));
            out.save(&output)?;
            if let Some(d) = debug {
                book_core::dewarp::debug_image(&img, &opts).save(d)?;
            }
            Ok(())
        }
    }
}

fn convert_one(input: &Path, output: &Path, opts: &ConvertOptions) -> Result<()> {
    eprintln!("{} -> {}", input.display(), output.display());
    let progress = |p: Progress| match p {
        Progress::PageProcessed { done, total, file } => eprintln!("  [{done}/{total}] {file}"),
        Progress::PageSharpened { done, total } => eprintln!("  sharpen [{done}/{total}]"),
        Progress::PageEncoded { done, total } if done == total => eprintln!("  PDF: {total} pages"),
        Progress::Finished { output } => eprintln!("  done: {}", output.display()),
        _ => {}
    };
    let report = convert_dir(input, output, opts, &progress)?;
    for (i, p) in report.pages.iter().enumerate() {
        let skew = p.deskew.as_ref().map(|d| format!("{:+.2}°", d.angle_deg)).unwrap_or_else(|| "-".into());
        let warp = p
            .dewarp
            .as_ref()
            .map(|d| {
                if d.applied {
                    format!("{:.0}px", d.max_displacement_px)
                } else {
                    d.message.clone()
                }
            })
            .unwrap_or_else(|| "-".into());
        eprintln!("  page {:>4}: skew {skew}, dewarp {warp}", i + 1);
    }
    if let Some(dir) = &opts.work_dir {
        std::fs::write(dir.join("report.json"), serde_json_string(&report))?;
    }
    Ok(())
}

// input 以下で画像を直接含むフォルダを 1 冊として、output/<相対パス>.pdf に変換する
fn convert_tree(input: &Path, output: &Path, opts: &ConvertOptions) -> Result<()> {
    let mut books = Vec::new();
    collect_books(input, output, &mut books)?;
    if books.is_empty() {
        bail!("no folders with images under '{}'", input.display());
    }
    let mut errors = 0;
    for dir in &books {
        let rel = dir.strip_prefix(input).unwrap();
        let name = if rel.as_os_str().is_empty() {
            PathBuf::from(input.file_name().unwrap_or_default())
        } else {
            rel.to_path_buf()
        };
        let pdf = output.join(name).with_extension("pdf");
        if let Err(e) = convert_one(dir, &pdf, opts) {
            eprintln!("  error: {e:#}");
            errors += 1;
        }
    }
    eprintln!("{} books, {} errors", books.len(), errors);
    if errors > 0 {
        bail!("{errors} book(s) failed");
    }
    Ok(())
}

fn collect_books(dir: &Path, output: &Path, books: &mut Vec<PathBuf>) -> Result<()> {
    if dir == output {
        return Ok(());
    }
    if !book_core::input::list_images(dir)?.is_empty() {
        books.push(dir.to_path_buf());
    }
    let mut subdirs: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !p.file_name().unwrap().to_string_lossy().starts_with(['_', '.']))
        .collect();
    subdirs.sort_by(|a, b| book_core::input::natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    for d in subdirs {
        collect_books(&d, output, books)?;
    }
    Ok(())
}

fn serde_json_string<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}
