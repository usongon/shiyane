# Shiyane (拾言)

Turn videos into bilingual SRT/VTT subtitles, or turn system audio/microphone into realtime subtitles. A local desktop app for macOS and Windows.

[中文文档](README.md) · [Latest release](https://github.com/usongon/shiyane/releases/latest)

## Screenshots

**File to subtitles**

<img src="docs/images/home-file-subtitle.png" width="720" alt="File-to-subtitle home">

**Realtime · audio source**

<img src="docs/images/realtime-source-select.png" width="720" alt="Realtime audio source picker">

**Realtime · source + translation**

<img src="docs/images/realtime-translation.png" width="720" alt="Realtime translation">

**Floating overlay · always on top**

<img src="docs/images/realtime-overlay.png" width="600" alt="Floating subtitle overlay">

## Features

- **File to subtitles** — drop a video (mp4, mkv, avi, mov, ...), pick the source language (or auto-detect), export SRT/VTT with both source and translated text. Up to 3 hours per video. Live per-sentence progress; tasks resume from where they stopped after a crash or restart (no re-upload or re-transcription); pause anytime, stop resets the task. Recently processed videos and their status are listed on the home screen for resuming or removal.
- **Realtime subtitles** — capture system audio (per-app selectable) or microphone, with live source text + translation; pause/resume/stop.
- **Floating subtitle overlay** — a separate always-on-top window in lyric-bar style, for watching videos or meetings without switching apps; adjustable background opacity.
- **Keys stay local** — API keys are encrypted on disk (AES-256-GCM + Argon2) and only ever sent to the respective APIs.
- **Audio is deleted after use** — audio is uploaded to your own OSS bucket via signed URLs and deleted once transcription finishes.

## Install

### macOS

Download the `.dmg` from [Releases](https://github.com/usongon/shiyane/releases) and drag the app into Applications. ffmpeg/ffprobe are bundled — no command-line tools needed.

### Windows

Download the `.exe` installer from [Releases](https://github.com/usongon/shiyane/releases) (NSIS, per-user install, no administrator required). Requires Windows 11 x64.

- First-run SmartScreen prompt: click "More info" → "Run anyway". The installer is unsigned; verify against the sha256 checksums published with the release.
- Known limitation: DRM content and a few apps (e.g. Teams meetings) cannot be captured per-process; choose "System Audio (All)" as the source instead.

## What you need

Recognition and translation run in the cloud, so prepare:

1. A [Bailian](https://bailian.console.aliyun.com/) API key (Beijing region) — speech recognition
2. An Alibaba Cloud [OSS](https://oss.console.aliyun.com/) bucket (private is fine) with a RAM AccessKey that can read/write it — audio relay, deleted after use
3. A translation API key — OpenAI / DashScope / DeepSeek / Kimi, or any OpenAI-compatible service

## Configuration

Open the **Settings** tab and fill in by group:

| Group | Field | Description |
|-------|-------|-------------|
| Speech recognition | API key / Workspace ID | Bailian (Beijing region); the workspace ID is in the console top-right |
| | File transcription model | Default `qwen-audio-3.0-asr-flash-filetrans` |
| | Realtime model | Default `qwen-audio-3.0-asr-flash-streaming` |
| Translation | Provider / API key | OpenAI / DashScope / DeepSeek / Kimi; model auto-filled per provider, editable |
| | Target language | Chinese / English / Japanese / Korean |
| Object storage | Endpoint / Bucket / AK | e.g. `oss-cn-hangzhou.aliyuncs.com` |
| | Path prefix | Optional, e.g. `shiyane-temp/` |

Speech-recognition and translation settings can be connectivity-tested before saving. The config file and encrypted keys live at:

- macOS: `~/Library/Application Support/com.usongon.shiyane/`
- Windows: `%APPDATA%\com.usongon.shiyane\`

## Build from source

```bash
# Rust (2024 edition) + Node.js >= 20 + Tauri CLI
cargo install tauri-cli

git clone https://github.com/usongon/shiyane.git
cd shiyane
cargo tauri build    # installs frontend deps and builds automatically; output in target/release/bundle/
```

- On macOS, optionally run `./scripts/build-ffmpeg.sh` to produce the bundled ffmpeg/ffprobe (arm64, LGPL); without it the app uses an ffmpeg from the system PATH.
- Windows builds require Windows 11 + VS Build Tools; fetch the ffmpeg sidecar with `scripts/fetch-ffmpeg-windows.sh`.

Third-party licenses: [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Development

```bash
cargo tauri dev                        # Vite dev server + Rust incremental build + HMR
cd src-ui && npm install && npm run dev   # frontend only, browser demo mode (mock data, no keys)
```

## Project layout

```
src/               # Rust backend: ASR (file/realtime), audio capture, OSS,
                   # translation, pipelines, checkpoint resume, subtitle
                   # generation, encrypted config, Tauri commands
src-ui/            # Frontend (React + TypeScript + Ant Design):
                   # file / realtime / overlay / settings views,
                   # Tauri bridge + browser mock demo layer
```

## License

[FSL-1.1-ALv2](LICENSE) — free for personal, internal commercial, educational and research use; using the software in a competing product or service requires a license. Each release automatically converts to Apache 2.0 two years after publication.
