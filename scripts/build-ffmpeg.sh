#!/bin/bash
# 构建 LGPL ffmpeg/ffprobe（macOS arm64，仅系统框架依赖，无 GPL 组件）
# 产物复制到 binaries/ 供 tauri externalBin 打包；版本固定保证可复现
set -euo pipefail

FFMPEG_VERSION="6.0"
TRIPLE="aarch64-apple-darwin"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT_DIR="$ROOT/binaries"
WORK="$(mktemp -d /tmp/ffmpeg-build.XXXXXX)"

cd "$WORK"
echo "== 下载 ffmpeg-${FFMPEG_VERSION} =="
curl -fSL -o ffmpeg.tar.xz "https://ffmpeg.org/releases/ffmpeg-${FFMPEG_VERSION}.tar.xz"
tar xf ffmpeg.tar.xz
cd "ffmpeg-${FFMPEG_VERSION}"

echo "== configure（LGPL：--disable-gpl --disable-nonfree；--disable-autodetect 隔离三方库）=="
./configure \
  --disable-gpl \
  --disable-nonfree \
  --disable-doc \
  --disable-debug \
  --disable-autodetect \
  --enable-static \
  --disable-shared \
  --enable-pthreads

echo "== make =="
make -j"$(sysctl -n hw.ncpu)"

mkdir -p "$OUT_DIR"
cp ffmpeg "$OUT_DIR/ffmpeg-${TRIPLE}"
cp ffprobe "$OUT_DIR/ffprobe-${TRIPLE}"

echo "== 完成 =="
ls -la "$OUT_DIR"
shasum -a 256 "$OUT_DIR/ffmpeg-${TRIPLE}" "$OUT_DIR/ffprobe-${TRIPLE}"
