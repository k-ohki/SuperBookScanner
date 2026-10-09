# SuperBookScanner for Mac (開発者向け)

使い方はリポジトリ直下の [`README.md`](../README.md)、仕様は [`../docs/mac_tauri_app_spec.md`](../docs/mac_tauri_app_spec.md) を参照。

## 構成

| パス | 内容 |
|---|---|
| `crates/book-core` | 画像処理パイプライン (純 Rust、OpenCV 不要)。UI に依存しないライブラリ |
| `crates/book-cli` | コマンドライン版 `superbook` |
| `app/src` | 画面 (TypeScript + React、Vite) |
| `app/src-tauri` | Tauri 2 のバックエンド。book-core を呼び、進捗をイベントで画面に送る |
| `models/uvdoc.onnx` | 写真の平面化モデル。`.app` の `Resources/models/` に同梱される (`tauri.conf.json` の `bundle.resources`) |
| `scripts/fetch-realesrgan.sh` | Real-ESRGAN (ncnn 版) を `third_party/realesrgan/` に取得する。`third_party/` は git 管理外 |

### book-core のモジュール

| モジュール | 内容 |
|---|---|
| `input` | 画像の列挙 (自然順)、EXIF 回転、heic の読み込み (`sips`) |
| `unwarp` | UVDoc (ONNX、tract で CPU 推論) による写真の平面化。紙の範囲・台形・反り・背景 |
| `split` | 見開き分割。ノドの影と本文の空白列から分割位置を探す |
| `deskew` | 傾き補正。投影プロファイルの分散が最大になる角度 (±5°) |
| `dewarp` | ノドの湾曲の補正。行をたどり多項式で近似し、変位場で展開 (旧 C# 版 `BookDewarp.cs` の移植) |
| `illumination` | 影・照明ムラの除去。紙の明るさを推定して割り算する |
| `layout` | 本文の範囲の検出と、全ページ共通の大きさ・余白での切り出し |
| `sharpen` | Real-ESRGAN を外部プロセスとして呼ぶ AI 鮮明化 |
| `pdf` | JPEG を再圧縮せずに埋め込む PDF 出力 (lopdf)。見開き表示・右綴じの設定 |
| `project` | 画像ごとの手動調整。入力フォルダの `superbook.json` に保存 |
| `pipeline` | 上記をつないだ変換処理。ページ単位で並列 (rayon)、進捗通知と中止に対応 |

## ビルド

```sh
# コアと CLI
cd mac
cargo build --release -p book-cli          # → target/release/superbook

# アプリ (.app と .dmg)
cd mac/app
npm ci
npm run tauri build                        # → target/release/bundle/{macos,dmg}/
npm run tauri build -- --bundles app       # .app だけ

# 開発中 (ホットリロード)
npm run tauri dev
```

## テスト

```sh
cd mac
cargo fmt --all --check
cargo test --release -p book-core -p book-cli
```

CI (`.github/workflows/mac-core.yml`) は、コアと CLI のテストを macOS と Linux で実行し、アプリのビルドを macOS で確認する。

## 外部ファイルの探し方

- **UVDoc モデル**: 環境変数 `SUPERBOOK_UVDOC` → 実行ファイルの隣 → `.app` の `Resources/models/` → `mac/models/`。見つからなければ平面化をとばす
- **Real-ESRGAN**: 環境変数 `SUPERBOOK_REALESRGAN` → 実行ファイルと同じフォルダ (とその下の `realesrgan/`) → `.app` の `Contents/Resources/realesrgan/` → 上位フォルダの `third_party/realesrgan/` (ビルドした `.app` からも `mac/third_party/` が見つかる) → PATH。CLI では `--realesrgan` で直接指定もできる

## デバッグ用の CLI コマンド

```sh
# 中間画像と各ページの補正結果 (report.json) を残す
./target/release/superbook convert photos/ -o out.pdf --work-dir work

# 写真の平面化だけを 1 枚で試す
./target/release/superbook unwarp photo.jpg out.png --rotate 270

# 見開き分割だけを試す
./target/release/superbook split spread.jpg out

# 歪み補正だけを試す (--debug で検出した行を描いた画像も出す)
./target/release/superbook dewarp page.jpg out.png --debug lines.png
```

## 注意点

- 低い解像度 (`--page-long-side` を小さくした場合など) で AI 鮮明化すると、小さな文字が別の字の形に変わることがある
- AI 鮮明化は GPU (Vulkan → MoltenVK → Metal) をほぼ使い切るので、処理中は画面が重くなる
- `npm run tauri build` で DMG を作るとき、作業用のディスクイメージ上でアプリを起動したままだと、取り出せずに失敗する (`Resource busy`)
