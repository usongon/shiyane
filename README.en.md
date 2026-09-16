# Shiyane (拾言)

A cross-platform desktop app that turns video files into translated subtitles. Drop a video, pick the language, export an SRT/VTT file with bilingual lines.

[中文文档](README.md)

## How it works

1. **Drop a video file** (mp4, mkv, avi, mov, ...)
2. **Pick the source language** (or auto-detect)
3. **Audio is extracted** locally via ffmpeg
4. **Speech recognition** — the audio is uploaded to your own OSS bucket and transcribed by [Alibaba Cloud Bailian](https://bailian.console.aliyun.com/) `qwen-audio-3.0-asr-flash-filetrans` (async task; up to 3 hours per video, 5GB extracted audio)
5. **Translation** — each sentence is translated via any OpenAI-compatible API (OpenAI, DashScope, DeepSeek, Kimi, ...)
6. **Export** — SRT or VTT with both source and translated text

## Features

- **File-to-subtitle pipeline** — drag & drop or file picker, live progress with per-sentence granularity, clear success/failure states with error messages
- **Resume & task control** — per-sentence progress is persisted; after a crash or restart, tasks resume from where they stopped (no re-upload or re-transcription). Pause anytime; stop resets the task
- **Recent tasks** — the home screen lists recently processed videos with their status; resume unfinished tasks or delete them
- **Real connectivity tests** — validate ASR / translate / OSS settings before saving
- **BYOK** — API keys are encrypted locally (AES-256-GCM + Argon2), only ever sent to the respective API
- **Private-by-default audio handling** — audio is uploaded to your own OSS bucket via signed URLs and deleted once transcription finishes
- **Separate file/realtime ASR models** — `qwen-audio-3.0-asr-flash-filetrans` for file transcription, `qwen-audio-3.0-asr-flash` reserved for the realtime mode
- **Cross-platform** — macOS, Windows, Linux (Tauri 2.0)

## Prerequisites

- [ffmpeg](https://ffmpeg.org/) and ffprobe on `PATH`
  - macOS: `brew install ffmpeg`
  - Ubuntu: `sudo apt install ffmpeg`
  - Windows: download from https://ffmpeg.org/download.html
- A [Bailian](https://bailian.console.aliyun.com/) API key (Beijing region)
- An Alibaba Cloud [OSS](https://oss.console.aliyun.com/) bucket (private is fine) with a RAM AccessKey that can read/write it

## Build from source

```bash
# Install Rust (2024 edition)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install Tauri CLI and Node.js (>= 20)
cargo install tauri-cli

# Clone and build
git clone https://github.com/usongon/shiyane.git
cd shiyane
cargo tauri build   # installs frontend deps in src-ui/ and builds it automatically
```

The built app is in `target/release/bundle/`.

## Development

```bash
cargo tauri dev    # Vite dev server (5173) + Rust incremental build + HMR
```

You can also run the frontend alone in a browser (demo mode with mock data, no keys required):

```bash
cd src-ui && npm install && npm run dev
```

## Configuration

Open the **Settings** tab:

| Field | Description |
|-------|-------------|
| **File transcription model** | Default `qwen-audio-3.0-asr-flash-filetrans` |
| **Realtime model** | Default `qwen-audio-3.0-asr-flash` (for realtime mode) |
| **ASR API Key** | Bailian API key |
| **Workspace ID** | Bailian workspace ID (required for Beijing region; console top-right) |
| **Translate Provider** | OpenAI / DashScope / DeepSeek / Kimi |
| **Translate Model** | Auto-filled per provider, editable |
| **Translate API Key** | Translation API key |
| **Target Language** | zh / en / ja / ko |
| **OSS Endpoint** | e.g. `oss-cn-hangzhou.aliyuncs.com` |
| **OSS Bucket / AK ID / AK Secret** | Your bucket and credentials |
| **OSS Path Prefix** | Optional, e.g. `shiyane-temp/` |

API keys are stored encrypted at `~/Library/Application Support/pick-up-sound-text/config.json` (0600 permissions).

## Architecture

```
src/               # Rust backend
├── asr/          # FileAsrProvider (async transcription) + DashScope realtime WebSocket client
├── audio/        # AudioSource trait + FileAudioSource (ffmpeg extraction)
├── oss/          # OSS uploader with HMAC-SHA1 signed URLs
├── translate/    # TranslateProvider trait + OpenAI-compatible HTTP client
├── pipeline/     # FilePipeline: extract → transcribe → translate → entries
├── checkpoint/   # resume: append-only progress log
├── subtitle/     # SubtitleEntry, SRT/VTT generation
├── config/       # AppConfig + encrypted keystore
└── commands/     # Tauri commands (frontend ↔ backend)

src-ui/            # Frontend (React + TypeScript)
├── src/lib/      # Tauri backend bridge + browser mock demo layer
├── src/views/    # File / Realtime / Settings views
└── src/theme.ts  # AntD theme (light/dark, brand tokens)
```

## Tech stack

- **Backend**: Rust 2024, Tauri 2.0, tokio, reqwest, tokio-tungstenite
- **Frontend**: React 18 + TypeScript + Ant Design 5 + Vite (light/dark themes)
- **ASR**: Bailian async file transcription API (submit → poll → download)
- **Translation**: OpenAI-compatible Chat Completions API
- **Audio**: ffmpeg (16kHz mono PCM WAV)

## License

MIT
