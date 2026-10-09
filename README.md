# SuperBookScanner

本のページを撮った写真やスキャン画像のフォルダから、読みやすく綺麗な PDF を作る Mac アプリです。

スマホで撮った見開きの写真を入れると、机や指などの背景を落とし、紙の台形・反り・ノドの湾曲を平らにします。さらに左右のページに分け、傾き・影を直して余白をそろえ、1 冊の PDF にまとめます。必要なら AI (Real-ESRGAN) で文字を鮮明にもできます。

- Apple Silicon の macOS で動くネイティブアプリ (Tauri 2 + Rust)。Python・OpenCV・CUDA は不要
- 自動で補正したあと、プレビューを見ながら画像ごとに手で直せる
- 同じ処理をコマンドライン (`superbook`) でも実行できる

## できること

| 段階 | 内容 | 手で直せること |
|---|---|---|
| 入力 | フォルダ内の画像をファイル名の順 (2 → 10) に並べる。EXIF の回転を反映し、A4 300dpi 相当の大きさにそろえる | 全体・画像ごとの回転、書き出さない画像の指定 |
| 写真の平面化 | 学習済みモデル UVDoc で、紙の範囲・台形・反りをまとめて直し、背景を落とす | オン/オフ (画像ごとにも) |
| 見開き分割 | ノドの影と本文の間の空白から分割位置を探し、左右 2 ページに分ける | 分割線をドラッグ、分けない |
| 傾き補正 | 文字の並びから ±5° の範囲で傾きを直す (縦書きにも対応) | オン/オフ |
| 歪み補正 | ノド付近で曲がった文字の行をまっすぐにする | オン/オフ (画像ごとにも) |
| 影・照明ムラの除去 | 紙の明るさのムラを取り、紙を白にそろえる | オン/オフ |
| 余白の統一 | 本文の範囲を見つけ、全ページを同じ大きさ・同じ余白で切り出す | 本文の枠をドラッグ、余白の大きさ |
| AI 鮮明化 (任意) | Real-ESRGAN で 4 倍に拡大してから 300 / 600dpi 相当に縮小し、文字の輪郭をなめらかにする | オン/オフ、解像度 |
| PDF 出力 | 各ページを JPEG で埋め込む。白黒のページはグレーで保存。見開き表示の設定付き | 右綴じ (縦書きの本)、画質 |

画像ごとの調整は、入力フォルダの `superbook.json` に保存されます。次にフォルダを開いたときや、CLI で変換するときにも使われます。

### 処理時間の目安

MacBook Pro (Apple Silicon) で、iPhone の写真 (3213×5712、見開き 2 枚 → 4 ページ) を処理した場合:

| 処理 | 時間 |
|---|---|
| 通常 (平面化・分割・各種補正・PDF) | 約 3 秒 (2 枚分) |
| AI 鮮明化つき (600dpi 相当) | 約 2 分 30 秒 (1 ページ約 40 秒、GPU を使用) |

AI 鮮明化は時間がかかり、処理中は GPU をほぼ使い切ります。まず鮮明化なしで仕上がりを確認するのがおすすめです。

## 動作環境

- macOS 13 以降、Apple Silicon (M1 以降)
- ビルドに必要なもの: [Rust](https://rustup.rs/) (stable)、Node.js 20 以降と npm、Xcode Command Line Tools (`xcode-select --install`)

## ビルドとインストール

```sh
git clone https://github.com/k-ohki/SuperBookScanner.git
cd SuperBookScanner/mac/app
npm ci
npm run tauri build
```

できあがるもの:

- アプリ: `mac/target/release/bundle/macos/SuperBookScanner.app`
- インストーラ: `mac/target/release/bundle/dmg/SuperBookScanner_<版>_aarch64.dmg`

`.app` を「アプリケーション」フォルダにコピーすれば使えます。写真の平面化モデル (`mac/models/uvdoc.onnx`) はアプリに同梱されます。

> 署名していないので、初回は Finder でアプリを右クリック →「開く」で起動してください。
> DMG を作るときに `hdiutil: couldn't unmount ... Resource busy` で失敗したら、ビルド中にマウントされたディスクイメージ上でアプリを起動していないか確認してください。そのアプリを終了し、`/Volumes/dmg.*` を取り出してからビルドし直してください。

### AI 鮮明化を使う場合

Real-ESRGAN (ncnn 版の `realesrgan-ncnn-vulkan`) は同梱していません。初回だけ次のスクリプトで取得してください (GitHub の [xinntao/Real-ESRGAN](https://github.com/xinntao/Real-ESRGAN) のリリース zip をダウンロードし、チェックサムを確かめてから `mac/third_party/realesrgan/` に置きます)。

```sh
./mac/scripts/fetch-realesrgan.sh
```

このリポジトリでビルドした `.app` は、ここに置いた実行ファイルを自動で見つけます。別の場所に置く場合は、環境変数 `SUPERBOOK_REALESRGAN` でその場所を指定してください。

## アプリの使い方

1. 「フォルダを開く」で、1 冊分のページ画像が入ったフォルダを選ぶ (ウィンドウへのドロップも可)。1 フォルダ = 1 冊です
2. 左の設定で全体の補正を選ぶ。横向きに撮った写真は「回転」で向きを直す
3. 下の一覧で画像を選ぶと、左に元の画像、右に補正後のページが表示される
   - 左の画像の縦線をドラッグ → 見開きの分割位置を直す
   - 右のページの青い枠をドラッグ → 本文の範囲 (余白の切り出し) を直す
   - 「この画像だけ」で、回転・平面化・歪み補正・分割をその画像だけ変える。不要な画像は「この画像を書き出さない」
4. 「PDF に書き出す」で保存先を選ぶ。進行状況が表示され、途中で中止もできる

## コマンドライン版

アプリと同じ処理をターミナルから実行できます。たくさんのフォルダをまとめて変換するときに便利です。

```sh
cd mac
cargo build --release

# 1 冊を PDF に (横向きに撮った写真なので左に 90° 回す)
./target/release/superbook convert ~/Photos/book1 -o ~/Books/book1.pdf --rotate 270

# AI 鮮明化も行う
./target/release/superbook convert ~/Photos/book1 -o ~/Books/book1.pdf --rotate 270 --sharpen

# サブフォルダごとに 1 冊ずつ、まとめて変換
./target/release/superbook convert ~/Photos -o ~/Books --recursive

# オプションの一覧
./target/release/superbook convert --help
```

主なオプション:

| オプション | 内容 |
|---|---|
| `--rotate 0/90/180/270` | 読み込んだ画像を時計回りに回す |
| `--rtl` | 右綴じ (縦書きの本) |
| `--sharpen` / `--sharpen-scale 1〜4` | AI 鮮明化と、その倍率 (2 で 600dpi 相当) |
| `--no-unwarp` / `--no-deskew` / `--no-dewarp` / `--no-illumination` / `--no-crop` | 各補正をしない (平らなスキャン画像なら `--no-unwarp --no-dewarp`) |
| `--margin 0.05` / `--quality 85` | 余白の大きさ / JPEG の画質 |
| `--no-project` | アプリで保存した画像ごとの調整 (`superbook.json`) を使わない |
| `--work-dir DIR` | 中間画像と各ページの補正結果 (`report.json`) を残す |

対応する画像形式: jpg / png / tif / webp / bmp / gif / heic (heic は macOS の `sips` で読み込み)。名前が `_` で始まるファイル・フォルダは無視します。

## リポジトリの構成

| パス | 内容 |
|---|---|
| [`mac/`](mac/) | アプリ・CLI・画像処理のソース一式。開発者向けの説明は [`mac/README.md`](mac/README.md) |
| [`docs/`](docs/) | 仕様 ([`mac_tauri_app_spec.md`](docs/mac_tauri_app_spec.md)) |
| [`.github/workflows/`](.github/workflows/) | CI (テストとアプリのビルド) |

## フォーク元について

このリポジトリは、登 大遊 氏の [DN_SuperBook_PDF_Converter](https://github.com/dnobori/DN_SuperBook_PDF_Converter) (Windows 用、C#) をフォークしたものです。フォーク元は「スキャンした書籍の PDF」を鮮明にするツールです。このプロジェクトでは目的を「本の写真・スキャン画像のフォルダから PDF を作る」に変え、Mac 版を Rust と Tauri で新しく作り直しました。Windows 版 (C#) のコードはこのリポジトリから削除しました。Windows 版 (ページ番号の検出、OCR など) を使いたい場合は、フォーク元を参照してください。

## ライセンス

[GNU AGPL v3](LICENSE) (フォーク元と同じ)。

同梱・利用している主なもの:

- [UVDoc](https://github.com/tanguymagne/UVDoc) (MIT): 写真の平面化モデル。ONNX に変換して `mac/models/uvdoc.onnx` として同梱
- [Real-ESRGAN](https://github.com/xinntao/Real-ESRGAN) (BSD-3-Clause): AI 鮮明化。同梱せず、利用者がスクリプトで取得する
- [tract](https://github.com/sonos/tract)、[image](https://github.com/image-rs/image)、[lopdf](https://github.com/J-F-Liu/lopdf)、[Tauri](https://tauri.app/) ほか (各ライセンスに従う)

## 使用上の注意

- 無保証です。大切な画像は、元のファイルを残したまま使ってください (アプリは入力フォルダに `superbook.json` 以外を書き込みません)
- 作った PDF は、ご自身で読むためだけに使ってください。本の著作権者に無断で配布・販売すると、著作権の侵害になります
