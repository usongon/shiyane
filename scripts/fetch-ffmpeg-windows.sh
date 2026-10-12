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
# git-bash/CI 环境可能缺 shasum（perl 脚本），回退 GNU sha256sum（coreutils 必有）
if command -v shasum >/dev/null 2>&1; then
  (cd "${TMP}" && grep " ${ASSET}\$" checksums.sha256 | shasum -a 256 -c -)
else
  (cd "${TMP}" && grep " ${ASSET}\$" checksums.sha256 | sha256sum -c -)
fi

# unzip 在 Windows runner 的 git-bash 缺失，回退 bsdtar。
# 注意：git-bash 的 tar 是 GNU tar（不识别 zip），必须显式用 System32 的 bsdtar
extract() {
  if command -v unzip >/dev/null 2>&1; then
    unzip -j -o "$1" "$2" -d "$3" >/dev/null
    return
  fi
  local bsdtar=""
  for cand in "/c/Windows/System32/tar.exe" "${SYSTEMROOT}\\System32\\tar.exe"; do
    if [ -x "$cand" ]; then bsdtar="$cand"; break; fi
  done
  [ -n "$bsdtar" ] || bsdtar="tar"
  local dest="$3"
  local tmpd
  tmpd="$(mktemp -d)"
  "$bsdtar" -xf "$1" -C "$tmpd"
  mv "$(find "$tmpd" -name "$(basename "$2")" -print -quit)" "$dest/$(basename "$2")"
  rm -rf "$tmpd"
}

echo ">> 解包并落位（tauri externalBin 三元组命名，Windows 必须带 .exe）"
mkdir -p "${OUT}" "${TMP}/bin"
extract "${TMP}/${ASSET}" "*/bin/ffmpeg.exe" "${TMP}/bin"
extract "${TMP}/${ASSET}" "*/bin/ffprobe.exe" "${TMP}/bin"
mv "${TMP}/bin/ffmpeg.exe"  "${OUT}/ffmpeg-x86_64-pc-windows-msvc.exe"
mv "${TMP}/bin/ffprobe.exe" "${OUT}/ffprobe-x86_64-pc-windows-msvc.exe"
ls -lh "${OUT}"/*x86_64-pc-windows-msvc.exe
echo ">> 完成"
