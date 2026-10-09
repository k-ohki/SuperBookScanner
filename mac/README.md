# SuperBookScanner — Mac 版

macOS 13 以降 (Apple Silicon) 用のアプリ。画像処理 (`../crates/`) と画面 (`../ui/`) は Windows 版と共通で、ここには Mac 用の設定だけを置く。

```sh
npm ci                  # ../ui の依存も入る
npm run tauri build     # → ../target/release/bundle/macos/SuperBookScanner.app と dmg/
./scripts/fetch-realesrgan.sh   # AI 鮮明化を使う場合 (初回だけ)
```

| パス | 内容 |
|---|---|
| `src-tauri/tauri.conf.json` | アプリの設定 (.app / .dmg、同梱するモデル) |
| `src-tauri/icons/` | アイコン (`icon.icns` ほか) |
| `src-tauri/capabilities/` | 画面から使える機能の許可 |
| `scripts/fetch-realesrgan.sh` | Real-ESRGAN を `../third_party/realesrgan/` に取得する (Linux の CLI でも使える) |

使い方は [`../README.md`](../README.md)、開発者向けの説明は [`../docs/development.md`](../docs/development.md)。
