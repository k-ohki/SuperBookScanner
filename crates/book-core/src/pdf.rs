//! PDF 出力。各ページを JPEG にして、再圧縮せずにそのまま (DCTDecode で) 埋め込む。

use anyhow::Result;
use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, RgbImage};
use lopdf::{dictionary, Document, Object, Stream};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PdfOptions {
    /// JPEG の品質 (1-100)
    pub jpeg_quality: u8,
    /// 解像度 (ページの物理サイズの計算に使う)
    pub dpi: f64,
    /// 右綴じ (縦書きの本)。見開きでページが右 → 左に進む
    pub right_to_left: bool,
    /// グレースケールのページを自動判定して、グレーの JPEG にする
    pub auto_grayscale: bool,
}

impl Default for PdfOptions {
    fn default() -> Self {
        Self {
            jpeg_quality: 85,
            dpi: 300.0,
            right_to_left: false,
            auto_grayscale: true,
        }
    }
}

/// 1 ページ分の、エンコード済みの画像。
pub struct EncodedPage {
    pub jpeg: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub gray: bool,
}

/// ページ画像を JPEG にする。
pub fn encode_page(img: &RgbImage, options: &PdfOptions) -> Result<EncodedPage> {
    let gray = options.auto_grayscale && is_grayscale(img);
    let mut jpeg = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(&mut jpeg, options.jpeg_quality.clamp(1, 100));
    if gray {
        let g = image::imageops::grayscale(img);
        enc.encode(g.as_raw(), g.width(), g.height(), ExtendedColorType::L8)?;
    } else {
        enc.encode(img.as_raw(), img.width(), img.height(), ExtendedColorType::Rgb8)?;
    }
    Ok(EncodedPage {
        jpeg,
        width: img.width(),
        height: img.height(),
        gray,
    })
}

/// 彩度のある画素がほとんどなければグレースケールとみなす。
pub fn is_grayscale(img: &RgbImage) -> bool {
    let mut colored = 0u64;
    let mut total = 0u64;
    for p in img.pixels().step_by(7) {
        let max = p[0].max(p[1]).max(p[2]) as i32;
        let min = p[0].min(p[1]).min(p[2]) as i32;
        if max - min > 40 {
            colored += 1;
        }
        total += 1;
    }
    total == 0 || (colored as f64) < total as f64 * 0.002
}

/// ページを順に並べた PDF を書き出す。
pub fn write_pdf(pages: &[EncodedPage], path: &Path, options: &PdfOptions) -> Result<()> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut kids: Vec<Object> = Vec::new();

    for page in pages {
        let mut image_stream = Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Image",
                "Width" => page.width as i64,
                "Height" => page.height as i64,
                "ColorSpace" => if page.gray { "DeviceGray" } else { "DeviceRGB" },
                "BitsPerComponent" => 8,
                "Filter" => "DCTDecode",
            },
            page.jpeg.clone(),
        );
        // JPEG はすでに圧縮済みなので、Flate で二重に圧縮しない
        image_stream.allows_compression = false;
        let image_id = doc.add_object(image_stream);

        let w_pt = page.width as f64 * 72.0 / options.dpi;
        let h_pt = page.height as f64 * 72.0 / options.dpi;
        let content = format!("q {w_pt:.3} 0 0 {h_pt:.3} 0 0 cm /Im0 Do Q");
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));

        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(w_pt as f32), Object::Real(h_pt as f32)],
            "Contents" => content_id,
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Im0" => image_id },
            },
        });
        kids.push(page_id.into());
    }

    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count,
        }),
    );

    // 見開き表示で 1 ページ目 (表紙) を単独で右側に置く。右綴じなら進む方向を右 → 左にする
    let mut catalog = dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
        "PageLayout" => "TwoPageRight",
    };
    if options.right_to_left {
        catalog.set("ViewerPreferences", dictionary! { "Direction" => "R2L" });
    }
    let catalog_id = doc.add_object(catalog);
    doc.trailer.set("Root", catalog_id);

    doc.save(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn writes_readable_pdf() {
        let dir = std::env::temp_dir().join(format!("book-core-pdf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.pdf");
        let opts = PdfOptions {
            right_to_left: true,
            ..Default::default()
        };
        let a = encode_page(&RgbImage::from_pixel(300, 600, Rgb([200, 200, 200])), &opts).unwrap();
        let b = encode_page(&RgbImage::from_pixel(300, 600, Rgb([200, 20, 20])), &opts).unwrap();
        assert!(a.gray && !b.gray);
        write_pdf(&[a, b], &path, &opts).unwrap();

        let doc = Document::load(&path).unwrap();
        assert_eq!(doc.get_pages().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
