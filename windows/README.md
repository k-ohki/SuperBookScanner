# SuperBookScanner — Windows 版

Windows 10 / 11 (64bit) 用のアプリ。画像処理 (`..\crates\`) と画面 (`..\ui\`) は Mac 版と共通で、ここには Windows 用の設定だけを置く。

ビルドには Rust (MSVC)、Node.js 20 以降、Visual Studio Build Tools の「C++ によるデスクトップ開発」が必要。

```powershell
npm ci                  # ..\ui の依存も入る
npm run tauri build     # → ..\target\release\bundle\nsis\SuperBookScanner_<版>_x64-setup.exe
powershell -ExecutionPolicy Bypass -File scripts\fetch-realesrgan.ps1   # AI 鮮明化を使う場合 (初回だけ)
```

| パス | 内容 |
|---|---|
| `src-tauri/tauri.conf.json` | アプリとインストーラ (NSIS、ユーザー単位、日本語) の設定、同梱するモデル |
| `src-tauri/icons/` | アイコン (`icon.ico` ほか) |
| `src-tauri/capabilities/` | 画面から使える機能の許可 |
| `scripts/fetch-realesrgan.ps1` | Real-ESRGAN (Windows 版) を `..\third_party\realesrgan\` に取得する。`-Dest` で置き場所を変えられる |

自分でビルドしなくても、GitHub Actions の `ci` ワークフロー (`app-windows`) がインストーラを成果物として保存している。

使い方は [`../README.md`](../README.md)、開発者向けの説明は [`../docs/development.md`](../docs/development.md)。
