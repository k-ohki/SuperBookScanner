# SuperBookScanner for Mac

本のページ画像 (スキャン・スマホ撮影) のフォルダから、読みやすい PDF を作るツール。
仕様は [`../docs/mac_tauri_app_spec.md`](../docs/mac_tauri_app_spec.md)。

現在は M2 まで (画像処理コア + CLI + AI 鮮明化)。Tauri アプリ (M3) はこれから。

## 構成

| パス | 内容 |
|---|---|
| `crates/book-core` | 画像処理パイプライン (純 Rust、OpenCV 不要) |
| `crates/book-cli` | コマンドライン版 `superbook` |

## 処理の流れ

1. 入力: フォルダ直下の画像をファイル名の自然順 (2 < 10) に並べ、EXIF の回転を反映し、A4 300dpi 相当の大きさに揃える
2. 傾き補正: 文字の投影が最も鋭くなる角度 (±5°) を探して回す。縦書きにも対応
3. 歪み補正: ノド付近の行の湾曲を直す (C# 版 PR #1 `BookDewarp.cs` の移植)
4. 影・照明ムラの除去: 紙の明るさを推定して割り算し、紙を白に揃える
5. 余白の統一: 本文の外側を白で埋め、全ページを同じ大きさ・同じ余白で切り出す
6. AI 鮮明化 (`--sharpen` のときだけ): Real-ESRGAN (ncnn 版) で 4 倍にしてから 2 倍 (600dpi 相当) に縮小する。Apple Silicon の GPU で動く
7. PDF 出力: JPEG をそのまま埋め込む。グレースケールのページは自動でグレーの JPEG にする

## 使い方

```sh
# Rust のインストール (未導入の場合)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

cd mac
cargo build --release

# 1 冊 (画像フォルダ 1 つ) を PDF に
./target/release/superbook convert ~/Scans/book1 -o ~/Books/book1.pdf

# AI 鮮明化も行う (初回だけ realesrgan-ncnn-vulkan を取得する)
./scripts/fetch-realesrgan.sh
./target/release/superbook convert ~/Scans/book1 -o ~/Books/book1.pdf --sharpen

# サブフォルダをまとめて (画像を含むフォルダごとに 1 冊)
./target/release/superbook convert ~/Scans -o ~/Books --recursive

# 主なオプション
#   --rotate 270          読み込み後に回す角度 (横向きに撮った写真など。時計回り)
#   --no-unwarp           写真の平面化 (紙の範囲・台形・反り・背景) をしない (平らなスキャンなど)
#   --no-project          アプリで保存したページごとの調整 (入力フォルダの superbook.json) を使わない
#   --rtl                 右綴じ (縦書きの本)
#   --sharpen             AI 鮮明化 (--sharpen-scale 2 で 600dpi 相当、--sharpen-model で モデル変更)
#   --no-dewarp           歪み補正をしない (平らなスキャンなど)
#   --margin 0.05         余白の大きさ
#   --quality 85          JPEG の品質
#   --work-dir DIR        中間画像と report.json (各ページの傾き・歪み補正の結果) を残す
./target/release/superbook convert --help

# 写真の平面化だけを 1 枚で試す
./target/release/superbook unwarp photo.jpg out.png --rotate 270

# 歪み補正だけを 1 枚で試す (--debug で検出した行を描いた画像を出力)
./target/release/superbook dewarp page.jpg out.png --debug lines.png
```

AI 鮮明化の実行ファイルは、`SUPERBOOK_REALESRGAN` 環境変数 → `superbook` と同じフォルダ → `mac/third_party/realesrgan/` → PATH の順に探す (`--realesrgan` で直接指定も可)。
低い解像度 (`--page-long-side` を小さくした場合など) で鮮明化すると、小さな文字が別の字の形に変わることがあるので注意。

写真の平面化には学習済みモデル UVDoc (MIT, https://github.com/tanguymagne/UVDoc) を ONNX に変換した `mac/models/uvdoc.onnx` を使う (Rust だけの ONNX ランタイム tract で CPU 推論、1 枚 1 秒ほど)。`SUPERBOOK_UVDOC` 環境変数 → 実行ファイルの隣 → .app の Resources → `mac/models/` の順に探す。見つからなければ平面化をとばす。

対応形式: jpg / png / tif / webp / bmp / gif / heic (heic は macOS の `sips` で変換)。`_` で始まるファイル・フォルダは無視する。

## テスト

```sh
cargo test
```
