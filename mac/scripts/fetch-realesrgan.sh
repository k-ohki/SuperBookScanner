#!/bin/sh
# AI 鮮明化に使う realesrgan-ncnn-vulkan (公式リリース v0.2.5.0) を third_party/realesrgan/ (リポジトリ直下) に取得する。
# Linux の CLI でも使える。Windows では windows/scripts/fetch-realesrgan.ps1 を使う。
# macOS 版は arm64 / x86_64 のユニバーサルバイナリで、Apple Silicon の GPU (Metal) で動く。
set -eu

cd "$(dirname "$0")/../.."
DEST=third_party/realesrgan
BASE=https://github.com/xinntao/Real-ESRGAN/releases/download/v0.2.5.0

case "$(uname -s)" in
  Darwin) ZIP=realesrgan-ncnn-vulkan-20220424-macos.zip
          SHA=e0ad05580abfeb25f8d8fb55aaf7bedf552c375b5b4d9bd3c8d59764d2cc333a ;;
  Linux)  ZIP=realesrgan-ncnn-vulkan-20220424-ubuntu.zip
          SHA= ;;
  *) echo "unsupported OS: $(uname -s)" >&2; exit 1 ;;
esac

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

echo "downloading $ZIP ..."
curl -fL --progress-bar -o "$TMP/$ZIP" "$BASE/$ZIP"

if [ -n "$SHA" ]; then
  echo "$SHA  $TMP/$ZIP" | shasum -a 256 -c -
fi

rm -rf "$DEST"
mkdir -p "$DEST"
unzip -q "$TMP/$ZIP" -d "$TMP/x"
cp "$TMP/x/realesrgan-ncnn-vulkan" "$DEST/"
cp -R "$TMP/x/models" "$DEST/"
chmod +x "$DEST/realesrgan-ncnn-vulkan"
if [ "$(uname -s)" = Darwin ]; then
  xattr -d com.apple.quarantine "$DEST/realesrgan-ncnn-vulkan" 2>/dev/null || true
fi

echo "installed: $DEST/realesrgan-ncnn-vulkan"
