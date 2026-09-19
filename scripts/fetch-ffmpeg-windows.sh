#!/usr/bin/env bash
# 拉取 Windows x64 LGPL 静态 ffmpeg/ffprobe（BtbN 构建产物，单文件 exe）
# 与 scripts/build-ffmpeg.sh（mac 自建）并列；产物不入库（/binaries/ 已 gitignore）
set -euo pipefail

ASSET="ffmpeg-n8.1-latest-win64-lgpl-8.1.zip"
BASE_URL="https://github.com/BtbN/FFmpeg-Builds/releases/download/latest"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${ROOT}/binaries"
TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

echo ">> 下载 ${ASSET}"
curl -fL --retry 3 -o "${TMP}/${ASSET}" "${BASE_URL}/${ASSET}"

echo ">> 下载并校验 checksums.sha256"
curl -fL --retry 3 -o "${TMP}/checksums.sha256" "${BASE_URL}/checksums.sha256"
(cd "${TMP}" && grep " ${ASSET}\$" checksums.sha256 | shasum -a 256 -c -)

echo ">> 解包并落位（tauri externalBin 三元组命名，Windows 必须带 .exe）"
unzip -j -o "${TMP}/${ASSET}" "*/bin/ffmpeg.exe" -d "${TMP}/bin" >/dev/null
unzip -j -o "${TMP}/${ASSET}" "*/bin/ffprobe.exe" -d "${TMP}/bin" >/dev/null
mkdir -p "${OUT}"
mv "${TMP}/bin/ffmpeg.exe"  "${OUT}/ffmpeg-x86_64-pc-windows-msvc.exe"
mv "${TMP}/bin/ffprobe.exe" "${OUT}/ffprobe-x86_64-pc-windows-msvc.exe"
ls -lh "${OUT}"/*x86_64-pc-windows-msvc.exe
echo ">> 完成"
