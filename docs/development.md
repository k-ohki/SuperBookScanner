# SuperBookScanner 開発者向けの説明

使い方はリポジトリ直下の [`README.md`](../README.md)、仕様は [`spec.md`](spec.md) を参照。

## 構成

画像処理・画面・アプリの中身は Mac 版と Windows 版で共通にし、OS ごとに違うもの (アプリの設定・アイコン・インストーラ・スクリプト) だけを `mac/` と `windows/` に分けている。

```
SuperBookScanner/
├─ Cargo.toml            Rust のワークスペース (target/ は直下にできる)
├─ crates/
│  ├─ book-core/         画像処理パイプライン (純 Rust、OpenCV 不要)。共通
│  ├─ book-cli/          コマンドライン版 superbook。共通
│  └─ book-app/          アプリの中身。共通。各 OS のアプリから run(context) を呼ぶ
│                        ops.rs (処理の本体) / lib.rs (Tauri のコマンド) / remote.rs (iPad などから使う Web サーバー)
├─ ui/                   画面 (TypeScript + React、Vite)。共通
├─ models/uvdoc.onnx     写真の平面化モデル。アプリに同梱する (tauri.conf.json の bundle.resources)
├─ mac/                  Mac 版
│  ├─ package.json       npm ci で ui/ の依存も入る (postinstall)。npm run tauri build でビルド
│  ├─ src-tauri/         Tauri のアプリ (main.rs・tauri.conf.json・アイコン・権限)。.app と .dmg を作る
│  └─ scripts/fetch-realesrgan.sh
├─ windows/              Windows 版
│  ├─ package.json
│  ├─ src-tauri/         Tauri のアプリ。NSIS のインストーラを作る
│  └─ scripts/fetch-realesrgan.ps1
├─ third_party/          取得スクリプトで入れる Real-ESRGAN (git 管理外)
└─ docs/
```

### book-core のモジュール

| モジュール | 内容 |
|---|---|
| `input` | 画像の列挙 (自然順)、EXIF 回転、heic の読み込み (Mac: `sips`、Windows: PowerShell から WIC) |
| `unwarp` | UVDoc (ONNX、tract で CPU 推論) による写真の平面化。紙の範囲・台形・反り・背景 |
| `split` | 見開き分割。ノドの影と本文の空白列から分割位置を探す |
| `deskew` | 傾き補正。投影プロファイルの分散が最大になる角度 (±5°) |
| `dewarp` | ノドの湾曲の補正。行をたどり多項式で近似し、変位場で展開 (旧 C# 版 `BookDewarp.cs` の移植) |
| `illumination` | 影・照明ムラの除去。紙の明るさを推定して割り算する |
| `layout` | 本文の範囲の検出と、全ページ共通の大きさ・余白での切り出し |
| `sharpen` | Real-ESRGAN を外部プロセスとして呼ぶ AI 鮮明化 |
| `pdf` | JPEG を再圧縮せずに埋め込む PDF 出力 (lopdf)。見開き表示・右綴じの設定 |
| `project` | 画像ごとの手動調整。入力フォルダの `superbook.json` に保存 |
| `process` | 外部プログラムの起動。Windows ではコンソール画面を出さない |
| `pipeline` | 上記をつないだ変換処理。ページ単位で並列 (rayon)、進捗通知と中止に対応 |

## iPad などから使う仕組み (remote.rs)

- アプリの中で axum の Web サーバーを `0.0.0.0:8765` で動かす。オン/オフは画面の「iPad から使う」(Tauri コマンド `remote_start` / `remote_stop`)
- 画面はアプリと同じ `ui/` をそのまま配る (Tauri の asset resolver から読むので、アプリに組み込まれたファイルが使われる)
- 画面側 (`ui/src/api.ts`) は、Tauri の中なら `invoke`、ブラウザなら `POST /api/<コマンド名>` (JSON) で同じ処理を呼ぶ。書き出しの進捗は Tauri のイベントのかわりに `GET /api/events` (Server-Sent Events)
- ブラウザだけの API: `books` (本の一覧)、`create_book`、`upload/{本}` (multipart。連番 0001.jpg… で保存)、`download/{本}` (PDF)
- 写真と PDF は「ライブラリ」(書類/SuperBookScanner/) に置き、ブラウザからのパスはすべてライブラリの中かを確かめる (認証はない)
- 動いている間は macOS の App Nap とスリープを止める (`NSProcessInfo.beginActivity`)。止めないと、アプリが裏に回ったときに処理が数倍遅くなる

## ビルド

```sh
# 共通部分と CLI (リポジトリ直下で)
cargo build --release -p book-cli          # → target/release/superbook

# Mac アプリ (.app と .dmg)
cd mac
npm ci
npm run tauri build                        # → target/release/bundle/{macos,dmg}/
npm run tauri build -- --bundles app       # .app だけ
npm run tauri dev                          # 開発中 (ホットリロード)

# Windows アプリ (インストーラ)
cd windows
npm ci
npm run tauri build                        # → target\release\bundle\nsis\
```

画面だけを変えるときは `ui/` で `npm run dev` (Vite) でも確認できるが、Tauri のコマンドは呼べないので、通常は `npm run tauri dev` を使う。

## テスト

```sh
# リポジトリ直下で
cargo fmt --all --check
cargo test --release -p book-core -p book-cli
```

CI (`.github/workflows/ci.yml`) は、画像処理と CLI のテストを macOS・Windows・Linux で実行する。Mac アプリは `.app` のビルドを確認し、Windows はインストーラを作って成果物 `SuperBookScanner-windows-installer` として保存する。

Windows ではビルドに Visual Studio Build Tools (C++) が必要 (tract がアセンブラのコードを含むため、Mac から Windows 向けにクロスビルドはできない)。

## 外部ファイルの探し方

- **UVDoc モデル**: 環境変数 `SUPERBOOK_UVDOC` → 実行ファイルの隣 (とその下の `models/`。Windows のインストール先はここ) → `.app` の `Resources/models/` → リポジトリ直下の `models/` (開発時)。見つからなければ平面化をとばす
- **Real-ESRGAN** (Windows では `realesrgan-ncnn-vulkan.exe`): 環境変数 `SUPERBOOK_REALESRGAN` → 実行ファイルと同じフォルダ (とその下の `realesrgan/`) → `.app` の `Contents/Resources/realesrgan/` → 上位フォルダの `third_party/realesrgan/` (ビルドした `.app` や `.exe` からもリポジトリ直下の `third_party/` が見つかる) → PATH。CLI では `--realesrgan` で直接指定もできる

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
- AI 鮮明化は GPU (Mac: Vulkan → MoltenVK → Metal、Windows: Vulkan) をほぼ使い切るので、処理中は画面が重くなる
- Windows の heic 読み込みには Microsoft Store の「HEIF 画像拡張機能」が必要。CI の Windows ランナーには入っていないので、heic のテストは Mac でしか行っていない
- `npm run tauri build` で DMG を作るとき、作業用のディスクイメージ上でアプリを起動したままだと、取り出せずに失敗する (`Resource busy`)
