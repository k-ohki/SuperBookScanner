# macOS 対応 仕様案

対象: Apple Silicon の MacBook Pro (ユニファイドメモリ 100GB 以上)、macOS 14 以降。Intel Mac は対象外。

必須要件: **AI 鮮明化** と **折り目 (ノド) の歪み補正** を macOS でも使えること。

## 1. 現状の Windows 依存箇所

| # | 箇所 | 内容 |
|---|------|------|
| 1 | `SuperBookToolsApp/SuperBookToolsApp/AiCommands.cs` (`SuperBookExternalTools`) | 外部ツールのパスが `..\external_tools\...\magick.exe` のように `\` 区切り + `.exe` で固定 |
| 2 | `internal_libs/.../Misc/MiscUtil.cs` `ExtractImagesFromPdfAsync` | 出力先を `dstDir + @"\page_%05d"` と `\` で連結 |
| 3 | `internal_libs/.../Misc/AiUtil.cs` `AiUtilBasicEngine.RunBatchCommandsDirectAsync` | Real-ESRGAN を `cmd.exe` + `.\venv\Scripts\activate` で起動 |
| 4 | `SuperBookTools/Basic/SuperPdfUtil.cs` `PdfYomitokuLib.RunBatchCommandsDirectAsync` | YomiToku を同じく `cmd.exe` + `venv\Scripts` で起動。既定デバイスが `cuda` |
| 5 | `SuperBookToolsApp.csproj` | `OpenCvSharp4.runtime.win` のみ参照。`Tesseract` NuGet は Windows 用ネイティブ DLL 同梱 |
| 6 | 全体 | `net6.0` (EOL)。Real-ESRGAN は CUDA GPU 前提 |

画像の鮮明化以外の処理 (ImageSharp による傾き補正・余白処理など) は純粋な .NET なので、そのまま動く見込み。

## 2. 方針

**「同じソース・同じコマンドで Windows と macOS の両方が動く」ことを目標にし、Windows の既存手順は壊さない。**

### 2.1 外部ツールの解決 (#1)

`SuperBookExternalTools` のパス固定をやめ、次の順で探す `ExternalToolResolver` を追加する。

1. 環境変数 (`SUPERBOOK_MAGICK`, `SUPERBOOK_QPDF`, `SUPERBOOK_PDFCPU`, `SUPERBOOK_EXIFTOOL`, `SUPERBOOK_REALESRGAN_DIR`, `SUPERBOOK_YOMITOKU_DIR`, `SUPERBOOK_TESSDATA_DIR`)
2. Windows: 従来どおり `external_tools/external_tools/image_tools/...` 配下 (`Path.Combine` で区切り文字を OS に合わせる)
3. macOS/Linux: `PATH` 上のコマンド (`/opt/homebrew/bin` を含む)

`mogrify` は ImageMagick 7 では `magick mogrify` で代用する。起動時に見つからないツールを一覧表示して終了する (`CheckEnv` コマンドを追加)。

### 2.2 パス区切り (#2)

`MiscUtil.cs` の `@"\page_%05d"` を `PP.Combine(dstDir, "page_%05d" + ext)` に置き換える。他にも `\` 直書きがないか grep で洗い出して同様に直す。

### 2.3 Python ツールの起動 (#3, #4)

`RunBatchCommandsDirectAsync` を OS で分岐させる。

- Windows: 現状どおり `cmd.exe` + `venv\Scripts\activate`
- macOS/Linux: `/bin/bash` に `source venv/bin/activate` + コマンドを標準入力で渡す。または venv の `venv/bin/python` を直接実行する (こちらの方がシンプルで推奨)

`internal_libs/IPA-DN-Cores` は外部ライブラリのコピーなので、変更箇所は最小限にしてコメントで明示する。

### 2.4 AI 鮮明化 (Real-ESRGAN) — 必須

Mac には CUDA がないので、PyTorch の **MPS (Metal) バックエンド** で既存の Python 版 Real-ESRGAN を動かす。

- 既存と同じモデル・同じスクリプトなので、Windows 版と出力が揃う
- 変更点は `inference_realesrgan.py` 呼び出し時のデバイス指定だけ (`RealESRGANer` に `device=torch.device("mps")` を渡す小さなパッチ。Real-ESRGAN 本体は `cuda` / `cpu` しか自動選択しないため)
- ユニファイドメモリが 100GB 以上あるので、`--tile 0` (分割なし) で 1 ページを丸ごと処理でき、タイル境界のつなぎ目も出ない。並列数もメモリに応じて増やせる
- 使っている演算 (畳み込み・補間・LeakyReLU) はすべて MPS 対応済みのはず (推測。着手時に実測で確認する)

予備案として [realesrgan-ncnn-vulkan](https://github.com/xinntao/Real-ESRGAN-ncnn-vulkan) の macOS 版バイナリ (Python 不要、MoltenVK 経由で GPU 動作) にも切り替えられるようにする。MPS 版で問題が出た場合に使う。

### 2.5 OCR

- ページ番号検出用 Tesseract: NuGet の `Tesseract` パッケージは macOS のネイティブライブラリを含まない。`brew install tesseract` で入る `libtesseract` / `libleptonica` を読み込めるよう `NativeLibrary.SetDllImportResolver` で解決する。うまくいかない場合は `TesseractOCRSharp` 等、macOS 対応のバインディングに置き換える。
- YomiToku: `Device` の既定値を OS で切り替える (Windows `cuda` / macOS `mps`、動かなければ `cpu`)。

### 2.6 OpenCV (#5)

`OpenCvSharp4.runtime.win` に加え、macOS 用ランタイムを条件付きで参照する。Apple Silicon 向けの公式 NuGet ランタイムがないため、`brew install opencv` + OpenCvSharpExtern を自前ビルドするか、[OpenCvSharp4.mini.runtime.osx-arm64](https://www.nuget.org/packages?q=opencvsharp+osx-arm64) 系のコミュニティパッケージを使う。**着手時に最初に検証すべき最大のリスク**。

### 2.7 .NET のバージョン (#6)

`net6.0` → `net8.0` に上げる (LTS。Apple Silicon ネイティブ対応)。`internal_libs` 側の csproj も合わせて変更する。

### 2.8 折り目 (ノド) の歪み補正 — 必須

別スレッドで実装済みの **draft PR #1** (`SuperBookTools/Basic/BookDewarp.cs`、`/dewarp:yes`、単体コマンド `DewarpImages`) をそのまま使う。

- ScanTailor の dewarping と同じ考え方 (テキスト行をたどって曲線を求め、変位場を作って面を展開する) を独自に実装したもの。ScanTailor のコードは移植していない
- OpenCvSharp と .NET だけで動くので、2.6 の OpenCV が macOS で読み込めれば追加作業はない
- 本プロジェクトとの統合: `ConvertImages` にも `/dewarp:yes|no` を追加する。**写真から取り込む場合は既定を有効** にする (ページが湾曲しているのが普通のため)。PDF 入力の既定は従来どおり無効
- PR #1 とこのブランチは `AiCommands.cs` の `ConvertPdf` を両方とも変更しているので、どちらかを先にマージしたら、もう一方で衝突を解消する

テキスト行が取れないページ (図だけのページ、写真集など) に備えて、将来 [UVDoc](https://github.com/tanguymagne/UVDoc) などの学習ベースの歪み補正を MPS で動かす選択肢も残しておく (今回はやらない)。

### 2.9 本の写真向けの前処理 (ScanTailor を参考に)

ScanTailor の処理段階 (向き補正 → ページ分割 → 傾き補正 → 本文領域の選択 → 余白 → 出力) と既存の処理を比べると、写真入力では次が足りない。

| ScanTailor の段階 | 現状 | 追加すること |
|---|---|---|
| Fix Orientation | EXIF 回転のみ反映 (ConvertImages で実装済み) | なし |
| Split Pages | なし | **見開き写真の左右分割**。ページの縦の綴じ目 (ノドの影と本文の空白列) を検出し、2 ページに分ける。`/split:auto\|yes\|no` |
| (ScanTailor にはない) | なし | **台形 (遠近) 補正**。撮影角度による台形の歪みを、紙の外形 (4 辺) を検出して透視変換で直す。外形が見つからなければ何もしない |
| Deskew | 実装済み | なし |
| Dewarping (Output 段階) | PR #1 | 2.8 のとおり |
| Select Content / Margins | 実装済み (ページ番号を使った位置合わせ・全ページ統一の余白) | なし。既存の方が ScanTailor より高機能 |
| Output (照明の均一化) | 色調整あり | **ノドの影・照明ムラの除去**。背景 (紙の色) を大きなぼかしで推定して割り算し、紙を白に揃える |

処理順は 入力の正規化 → 台形補正 → 見開き分割 → 傾き補正 → 歪み補正 → 影の除去 → AI 鮮明化 → 余白・位置合わせ → PDF 化 とする。追加 3 つも OpenCV だけで実装でき、プラットフォームに依存しない。

## 3. macOS でのセットアップ手順 (想定)

```sh
brew install dotnet@8 python@3.11 imagemagick ghostscript qpdf pdfcpu exiftool tesseract tesseract-lang opencv
# AI 鮮明化
#   Real-ESRGAN を clone して venv に torch (MPS 対応版) を入れ、SUPERBOOK_REALESRGAN_DIR に指定
# OCR (任意)
python3 -m venv ~/yomitoku/venv && ~/yomitoku/venv/bin/pip install yomitoku
export SUPERBOOK_YOMITOKU_DIR=~/yomitoku

dotnet run --project SuperBookToolsApp -- /CMD ConvertImages ~/Scans/book1 /dst:~/Books /dewarp:yes /ocr:no
```

## 4. 作業順序

1. OpenCvSharp と Tesseract が Apple Silicon で読み込めるかを検証 (最大のリスク)
2. Real-ESRGAN が MPS で動くか、速度と出力を Windows 版と比べて検証
3. `net8.0` 化
4. 外部ツールの解決 (2.1) とパス区切りの修正 (2.2)
5. Python ツール起動の OS 分岐 (2.3) と MPS 対応 (2.4)
6. PR #1 の歪み補正をマージし、`ConvertImages` に `/dewarp` を追加 (2.8)
7. 写真向け前処理: 影の除去 → 見開き分割 → 台形補正 の順 (2.9。効果が大きく簡単なものから)
8. YomiToku の `mps` 対応 (2.5)
9. README に macOS 手順を追加し、GitHub Actions の `macos-14` ランナーでビルド確認

1〜2 で動かないと分かった場合は、予備案 (ncnn 版、コミュニティ版 OpenCV ランタイム) に切り替える。
