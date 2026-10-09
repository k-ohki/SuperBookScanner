//! ページごとの手動調整。入力フォルダの superbook.json に保存し、書き出し時にも使う。
//! (ScanTailor のプロジェクトファイルに相当する。自動処理の結果を、その画像についてだけ上書きする)

use crate::layout::Rect;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const PROJECT_FILE: &str = "superbook.json";

/// 見開き分割の手動指定。
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SplitOverride {
    /// 分割しない
    None,
    /// 幅に対する割合 (0..1) の位置で分割する
    At(f64),
}

/// 0..1 に正規化した矩形 (補正後のページ画像に対する割合)。
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl RectF {
    pub fn to_px(&self, w: u32, h: u32) -> Option<Rect> {
        let x0 = (self.x.clamp(0.0, 1.0) * w as f64).round() as u32;
        let y0 = (self.y.clamp(0.0, 1.0) * h as f64).round() as u32;
        let x1 = ((self.x + self.w).clamp(0.0, 1.0) * w as f64).round() as u32;
        let y1 = ((self.y + self.h).clamp(0.0, 1.0) * h as f64).round() as u32;
        (x1 > x0 && y1 > y0).then(|| Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        })
    }
}

/// 1 枚の入力画像に対する手動調整。None の項目は全体の設定・自動処理に従う。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PageOverride {
    /// 書き出さない
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub skip: bool,
    /// 回転 (時計回り)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotate: Option<u16>,
    /// 写真の平面化をするか
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unwarp: Option<bool>,
    /// 歪み補正をするか
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dewarp: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split: Option<SplitOverride>,
    /// 本文の範囲 (分割後のページごと。左 = 0 / 右 = 1)。余白をそろえる基準になる
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub content: BTreeMap<u8, RectF>,
}

impl PageOverride {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    /// キーは入力フォルダからの相対パス ("/" 区切り)
    pub pages: BTreeMap<String, PageOverride>,
}

impl Project {
    /// フォルダの superbook.json を読む。なければ空。
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(PROJECT_FILE);
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)?;
        serde_json::from_str(&text).with_context(|| format!("{} を読めません", path.display()))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = dir.join(PROJECT_FILE);
        if self.pages.is_empty() {
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            return Ok(());
        }
        std::fs::write(&path, serde_json::to_string_pretty(self)? + "\n").with_context(|| format!("{} に書けません", path.display()))
    }

    pub fn key(dir: &Path, file: &Path) -> String {
        let rel = file.strip_prefix(dir).unwrap_or(file);
        rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
    }

    pub fn get(&self, dir: &Path, file: &Path) -> PageOverride {
        self.pages.get(&Self::key(dir, file)).cloned().unwrap_or_default()
    }

    pub fn set(&mut self, dir: &Path, file: &Path, o: PageOverride) {
        let key = Self::key(dir, file);
        if o.is_empty() {
            self.pages.remove(&key);
        } else {
            self.pages.insert(key, o);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_empty_removes_file() {
        let dir = std::env::temp_dir().join(format!("superbook-project-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let file = dir.join("sub").join("a.jpg");
        let mut p = Project::default();
        let mut o = PageOverride {
            split: Some(SplitOverride::At(0.48)),
            ..Default::default()
        };
        o.content.insert(
            1,
            RectF {
                x: 0.1,
                y: 0.1,
                w: 0.8,
                h: 0.8,
            },
        );
        p.set(&dir, &file, o.clone());
        p.save(&dir).unwrap();
        assert!(dir.join(PROJECT_FILE).is_file());
        let q = Project::load(&dir).unwrap();
        assert_eq!(q.get(&dir, &file), o);
        assert_eq!(q.pages.keys().next().unwrap(), "sub/a.jpg");

        p.set(&dir, &file, PageOverride::default());
        p.save(&dir).unwrap();
        assert!(!dir.join(PROJECT_FILE).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rect_to_px() {
        let r = RectF {
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.25,
        }
        .to_px(400, 800)
        .unwrap();
        assert_eq!((r.x, r.y, r.w, r.h), (100, 400, 200, 200));
        assert!(RectF {
            x: 0.5,
            y: 0.5,
            w: 0.0,
            h: 0.1
        }
        .to_px(10, 10)
        .is_none());
    }
}
