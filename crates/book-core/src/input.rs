//! 入力: 画像フォルダの列挙と、1 ページ分の画像の読み込み・正規化。

use anyhow::{bail, Context, Result};
use image::{imageops, DynamicImage, ImageDecoder, ImageReader, RgbImage, RgbaImage};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// 対応する画像の拡張子 (小文字)。heic/heif は OS の機能 (macOS: `sips`、Windows: WIC) で変換して読む。
pub const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp", "bmp", "gif", "heic", "heif"];

/// フォルダ直下の画像ファイルを、ページ順 (ファイル名の自然順: 2.jpg < 10.jpg) に列挙する。
/// `_` や `.` で始まるファイルは無視する。
pub fn list_images(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read directory '{}'", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            !name.starts_with('_') && !name.starts_with('.') && is_supported(p)
        })
        .collect();
    files.sort_by(|a, b| natural_cmp(&a.file_name().unwrap().to_string_lossy(), &b.file_name().unwrap().to_string_lossy()));
    Ok(files)
}

fn is_supported(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| SUPPORTED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// ファイル名中の数字を数値として比較する (大文字小文字は区別しない)。
pub fn natural_cmp(x: &str, y: &str) -> Ordering {
    let (xb, yb) = (x.as_bytes(), y.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < xb.len() && j < yb.len() {
        if xb[i].is_ascii_digit() && yb[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < xb.len() && xb[i].is_ascii_digit() {
                i += 1;
            }
            while j < yb.len() && yb[j].is_ascii_digit() {
                j += 1;
            }
            let nx = x[si..i].trim_start_matches('0');
            let ny = y[sj..j].trim_start_matches('0');
            let ord = nx.len().cmp(&ny.len()).then_with(|| nx.cmp(ny));
            if ord != Ordering::Equal {
                return ord;
            }
        } else {
            let ord = xb[i].to_ascii_uppercase().cmp(&yb[j].to_ascii_uppercase());
            if ord != Ordering::Equal {
                return ord;
            }
            i += 1;
            j += 1;
        }
    }
    (xb.len() - i).cmp(&(yb.len() - j)).then_with(|| x.cmp(y))
}

/// 画像を読み込み、EXIF の回転を反映し、透過部分を白にして RGB で返す。
pub fn load_page(path: &Path) -> Result<RgbImage> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext == "heic" || ext == "heif" {
        return load_heic(path);
    }

    let mut decoder = ImageReader::open(path)
        .with_context(|| format!("cannot open '{}'", path.display()))?
        .with_guessed_format()?
        .into_decoder()
        .with_context(|| format!("cannot decode '{}'", path.display()))?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder).with_context(|| format!("cannot decode '{}'", path.display()))?;
    img.apply_orientation(orientation);
    Ok(flatten_to_white(img))
}

fn flatten_to_white(img: DynamicImage) -> RgbImage {
    if !img.color().has_alpha() {
        return img.to_rgb8();
    }
    let rgba: RgbaImage = img.to_rgba8();
    let mut out = RgbImage::new(rgba.width(), rgba.height());
    for (o, p) in out.pixels_mut().zip(rgba.pixels()) {
        let a = p[3] as u32;
        for c in 0..3 {
            o[c] = ((p[c] as u32 * a + 255 * (255 - a)) / 255) as u8;
        }
    }
    out
}

// HEIC は image crate で読めないので、OS の機能で PNG に変換してから読む。
fn load_heic(path: &Path) -> Result<RgbImage> {
    let tmp = std::env::temp_dir().join(format!("superbook-heic-{}-{}.png", std::process::id(), rand_suffix(path)));
    let converted = convert_heic_to_png(path, &tmp);
    let img = converted.and_then(|_| load_page(&tmp));
    let _ = std::fs::remove_file(&tmp);
    img
}

// macOS: 標準の sips を使う
#[cfg(target_os = "macos")]
fn convert_heic_to_png(src: &Path, dst: &Path) -> Result<()> {
    let out = crate::process::command("sips")
        .args(["-s", "format", "png"])
        .arg(src)
        .arg("--out")
        .arg(dst)
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        _ => bail!("cannot convert HEIC '{}' (requires macOS 'sips')", src.display()),
    }
}

// Windows: PowerShell から Windows の画像コーデック (WIC) で読む。
// Microsoft Store の「HEIF 画像拡張機能」が入っている必要がある。
#[cfg(windows)]
fn convert_heic_to_png(src: &Path, dst: &Path) -> Result<()> {
    const SCRIPT: &str = "$ErrorActionPreference='Stop'; Add-Type -AssemblyName PresentationCore; \
        $s=[IO.File]::OpenRead($env:SUPERBOOK_SRC); \
        try { $d=[Windows.Media.Imaging.BitmapDecoder]::Create($s,'None','OnLoad'); \
              $e=New-Object Windows.Media.Imaging.PngBitmapEncoder; \
              $e.Frames.Add([Windows.Media.Imaging.BitmapFrame]::Create($d.Frames[0])); \
              $o=[IO.File]::Create($env:SUPERBOOK_DST); try { $e.Save($o) } finally { $o.Close() } } \
        finally { $s.Close() }";
    let out = crate::process::command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("SUPERBOOK_SRC", src)
        .env("SUPERBOOK_DST", dst)
        .output();
    match out {
        Ok(o) if o.status.success() && dst.is_file() => Ok(()),
        _ => bail!(
            "cannot convert HEIC '{}'. Install \"HEIF Image Extensions\" from the Microsoft Store, or convert the photos to JPEG",
            src.display()
        ),
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn convert_heic_to_png(src: &Path, _dst: &Path) -> Result<()> {
    bail!("cannot read HEIC '{}' on this OS. Convert the photos to JPEG", src.display())
}

fn rand_suffix(path: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}

/// 時計回りに 90 度単位で回す (それ以外の角度はそのまま返す)。
pub fn rotate_cw(img: RgbImage, degrees: u16) -> RgbImage {
    match degrees % 360 {
        90 => imageops::rotate90(&img),
        180 => imageops::rotate180(&img),
        270 => imageops::rotate270(&img),
        _ => img,
    }
}

/// 縦横比を保ったまま、`max_w` x `max_h` の枠に収まるよう拡大・縮小する
/// (C# 版の ImageMagick `-resize WxH` と同じ。既定は A4 300dpi の 2480x3508)。
pub fn fit_to_box(img: &RgbImage, max_w: u32, max_h: u32) -> RgbImage {
    let (w, h) = img.dimensions();
    let s = (max_w as f64 / w as f64).min(max_h as f64 / h as f64);
    let nw = ((w as f64 * s).round() as u32).max(1);
    let nh = ((h as f64 * s).round() as u32).max(1);
    if (nw, nh) == (w, h) {
        return img.clone();
    }
    imageops::resize(img, nw, nh, imageops::FilterType::Lanczos3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["p10.jpg", "p2.jpg", "P1.jpg", "p02b.jpg", "a.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["a.jpg", "P1.jpg", "p2.jpg", "p02b.jpg", "p10.jpg"]);
    }
}
