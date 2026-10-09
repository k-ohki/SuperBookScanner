# macOS 対応 仕様案

対象: Apple Silicon (M1 以降) の macOS 14 以降。Intel Mac は「動けば良い」扱いとする。

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

### 2.4 AI 鮮明化 (Real-ESRGAN)

Mac には CUDA がないため、次のどちらかにする。

- **案 A (推奨)**: [realesrgan-ncnn-vulkan](https://github.com/xinntao/Real-ESRGAN-ncnn-vulkan) の macOS 版バイナリを使う。Python 不要で、Metal (MoltenVK) により GPU で動き、導入が簡単。出力画質は PyTorch 版とほぼ同等。
- 案 B: 既存の PyTorch 版を `--device mps` 相当で動かす。Real-ESRGAN 本体は MPS を正式サポートしていないため、パッチが必要になる可能性がある。

案 A を採用し、`AiUtilRealEsrganEngine` とは別に `RealEsrganNcnnEngine` を作って、macOS では自動でこちらを使う。オプション `/sharpen:yes|no` を追加し、AI 鮮明化を省略して高速に処理することもできるようにする (既存の `SkipRealesrgan` を使う)。

### 2.5 OCR

- ページ番号検出用 Tesseract: NuGet の `Tesseract` パッケージは macOS のネイティブライブラリを含まない。`brew install tesseract` で入る `libtesseract` / `libleptonica` を読み込めるよう `NativeLibrary.SetDllImportResolver` で解決する。うまくいかない場合は `TesseractOCRSharp` 等、macOS 対応のバインディングに置き換える。
- YomiToku: `Device` の既定値を OS で切り替える (Windows `cuda` / macOS `mps`、動かなければ `cpu`)。

### 2.6 OpenCV (#5)

`OpenCvSharp4.runtime.win` に加え、macOS 用ランタイムを条件付きで参照する。Apple Silicon 向けの公式 NuGet ランタイムがないため、`brew install opencv` + OpenCvSharpExtern を自前ビルドするか、[OpenCvSharp4.mini.runtime.osx-arm64](https://www.nuget.org/packages?q=opencvsharp+osx-arm64) 系のコミュニティパッケージを使う。**着手時に最初に検証すべき最大のリスク**。

### 2.7 .NET のバージョン (#6)

`net6.0` → `net8.0` に上げる (LTS。Apple Silicon ネイティブ対応)。`internal_libs` 側の csproj も合わせて変更する。

## 3. macOS でのセットアップ手順 (想定)

```sh
brew install dotnet@8 imagemagick ghostscript qpdf pdfcpu exiftool tesseract tesseract-lang opencv
# AI 鮮明化
#   realesrgan-ncnn-vulkan の macOS 版 zip を展開し、SUPERBOOK_REALESRGAN_DIR に指定
# OCR (任意)
python3 -m venv ~/yomitoku/venv && ~/yomitoku/venv/bin/pip install yomitoku
export SUPERBOOK_YOMITOKU_DIR=~/yomitoku

dotnet run --project SuperBookToolsApp -- /CMD ConvertImages ~/Scans/book1 /dst:~/Books /ocr:no
```

## 4. 作業順序

1. OpenCvSharp と Tesseract が Apple Silicon で読み込めるかを検証 (最大のリスク)
2. `net8.0` 化
3. 外部ツールの解決 (2.1) とパス区切りの修正 (2.2)
4. Python ツール起動の OS 分岐 (2.3)
5. realesrgan-ncnn-vulkan 対応 (2.4)
6. YomiToku の `mps` 対応 (2.5)
7. README に macOS 手順を追加し、GitHub Actions の `macos-14` ランナーでビルド確認

## 5. 確認したいこと

- Mac の機種 (Apple Silicon か) とメモリ容量
- AI 鮮明化は必須か (なしでも良ければ 2.4 を後回しにして早く動かせる)
