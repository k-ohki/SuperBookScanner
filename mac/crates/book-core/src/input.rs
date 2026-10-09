//! 入力: 画像フォルダの列挙と、1 ページ分の画像の読み込み・正規化。

use anyhow::{bail, Context, Result};
use image::{imageops, DynamicImage, ImageDecoder, ImageReader, RgbImage, RgbaImage};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};

/// 対応する画像の拡張子 (小文字)。heic/heif は macOS の `sips` で変換して読む。
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

// macOS 標準の sips で PNG に変換してから読む (image crate は HEIC を読めないため)
fn load_heic(path: &Path) -> Result<RgbImage> {
    let tmp = std::env::temp_dir().join(format!("superbook-heic-{}-{}.png", std::process::id(), rand_suffix(path)));
    let status = std::process::Command::new("sips")
        .args(["-s", "format", "png"])
        .arg(path)
        .arg("--out")
        .arg(&tmp)
        .output();
    match status {
        Ok(o) if o.status.success() => {}
        _ => bail!("cannot convert HEIC '{}' (requires macOS 'sips')", path.display()),
    }
    let img = load_page(&tmp);
    let _ = std::fs::remove_file(&tmp);
    img
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
