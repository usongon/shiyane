# 拾言 (Shiyane)

视频转双语字幕 + 实时字幕工具。拖入视频文件导出 SRT/VTT，或监听系统音频/麦克风实时出字幕。

[English](README.en.md)

## 预览

| 文件转字幕 | 实时字幕 · 音源选择 |
|:---:|:---:|
| <img src="docs/images/home-file-subtitle.png" width="400" alt="文件转字幕首页"> | <img src="docs/images/realtime-source-select.png" width="400" alt="实时字幕音源选择"> |
| **实时字幕 · 原文与译文** | **悬浮字幕 · 置顶歌词条** |
| <img src="docs/images/realtime-translation.png" width="400" alt="实时字幕翻译效果"> | <img src="docs/images/realtime-overlay.png" width="400" alt="桌面悬浮字幕"> |

## 工作流程

1. **拖入视频文件**（支持 mp4、mkv、avi、mov 等格式）
2. **选择视频语言**（或自动识别）
3. **本地提取音频** — 通过 ffmpeg
4. **语音识别** — 音频上传到你自己的 OSS Bucket，由[阿里云百炼](https://bailian.console.aliyun.com/) `qwen-audio-3.0-asr-flash-filetrans` 异步转写（单视频最长 3 小时，提取音频最大 5GB）
5. **翻译** — 逐句调用任何 OpenAI 兼容 API（OpenAI、百炼、DeepSeek、Kimi 等）
6. **导出** — SRT 或 VTT，包含原文与译文

## 特性

- **文件转字幕流水线** — 拖拽或点击选择，逐句实时进度，成功/失败状态与错误信息明确展示
- **实时字幕** — 监听系统音频（可按进程选择）或麦克风，边说边出原文与译文；支持暂停/恢复/停止
- **悬浮字幕窗** — 置顶歌词条式悬浮窗，看片/开会时无需切换窗口；背景不透明度可调
- **断点续传与任务控制** — 进度逐句保存，应用崩溃或重启后从断点继续（不重复上传与转写）；支持随时暂停，停止则清零重跑
- **最近任务** — 主页记录最近处理的视频与各自状态，可继续未完成的任务或删除记录
- **真实连通性测试** — 保存前即可验证 ASR / 翻译 / OSS 配置是否可用
- **BYOK（自带密钥）** — API Key 本地加密存储（AES-256-GCM + Argon2），只发送到对应 API
- **音频隐私优先** — 音频通过签名 URL 上传到你自己的 OSS Bucket，转写完成后自动删除
- **文件/实时 ASR 模型分开配置** — 文件转写用 `qwen-audio-3.0-asr-flash-filetrans`，实时识别用 `qwen-audio-3.0-asr-flash-streaming`
- **跨平台** — macOS、Windows、Linux（基于 Tauri 2.0）

## 前置要求

下载安装包的用户**开箱即用**——macOS 发行包已内置 ffmpeg/ffprobe，无需额外安装。

只需准备：

- 一个[百炼](https://bailian.console.aliyun.com/) API Key（北京区域）
- 一个阿里云 [OSS](https://oss.console.aliyun.com/) Bucket（私有即可）及具有读写权限的 RAM AccessKey

从源码构建时，若未将内置二进制打包（见下），需自行安装 ffmpeg：

```bash
# macOS
brew install ffmpeg

# Ubuntu
sudo apt install ffmpeg

# Windows：从 https://ffmpeg.org/download.html 下载
```

## 从源码构建

```bash
# 安装 Rust（2024 edition）
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 安装 Tauri CLI 与 Node.js（≥ 20）
cargo install tauri-cli

# 克隆并构建
git clone https://github.com/usongon/shiyane.git
cd shiyane
./scripts/build-ffmpeg.sh   # 可选：生成捆绑用的 ffmpeg/ffprobe（macOS arm64，LGPL）
cargo tauri build           # 会自动在 src-ui/ 安装依赖并构建前端
```

不运行 `build-ffmpeg.sh` 也可构建，此时应用回落到系统 PATH 中的 ffmpeg。

构建产物在 `target/release/bundle/` 目录下。第三方组件许可见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

## 开发模式

```bash
cargo tauri dev    # Vite dev server (5173) + Rust 增量编译 + 热更新
```

也可以只起前端在浏览器里预览界面（自动进入演示模式，使用模拟数据，不需要任何密钥）：

```bash
cd src-ui && npm install && npm run dev
```

## 配置说明

打开应用的**设置**页：

| 字段 | 说明 |
|------|------|
| **文件转写模型** | 默认 `qwen-audio-3.0-asr-flash-filetrans` |
| **实时识别模型** | 默认 `qwen-audio-3.0-asr-flash-streaming`（供实时字幕使用） |
| **ASR API Key** | 百炼 API Key |
| **业务空间 ID** | 百炼 Workspace ID（北京区域必填，控制台右上角获取） |
| **翻译渠道** | OpenAI / 百炼 / DeepSeek / Kimi |
| **翻译模型** | 根据渠道自动填充，可修改 |
| **翻译 API Key** | 翻译 API Key |
| **目标语言** | 中文 / 英文 / 日文 / 韩文 |
| **OSS Endpoint** | 例如 `oss-cn-hangzhou.aliyuncs.com` |
| **OSS Bucket / AK ID / AK Secret** | 你的 Bucket 与访问凭证 |
| **OSS 路径前缀** | 可选，例如 `shiyane-temp/` |

API Key 加密存储在 `~/Library/Application Support/pick-up-sound-text/config.json`（0600 权限）。

## 架构

```
src/               # Rust 后端
├── asr/          # 文件转写抽象（异步任务）+ 百炼实时 WebSocket 客户端
├── audio/        # 音频源抽象 + 文件音频源（ffmpeg）+ 采集抽象（ScreenCaptureKit / cpal）
├── oss/          # OSS 上传 + HMAC-SHA1 签名 URL
├── translate/    # 翻译抽象 + OpenAI 兼容 HTTP 客户端
├── pipeline/     # 文件流水线 + 实时管线（采集 → ASR → 翻译，三并发 task）
├── checkpoint/   # 断点续传：追加式进度日志
├── subtitle/     # 字幕条目、SRT/VTT 生成
├── config/       # 配置管理 + 加密密钥存储
└── commands/     # Tauri 命令（前端 ↔ 后端）

src-ui/            # 前端（React + TypeScript）
├── src/lib/      # Tauri 后端桥接层 + 浏览器 mock 演示层
├── src/views/    # 文件转字幕 / 实时字幕 / 悬浮字幕窗 / 设置
└── src/theme.ts  # AntD 主题（浅/暗双主题，品牌色 token）
```

## 技术栈

- **后端**：Rust 2024、Tauri 2.0、tokio、reqwest、tokio-tungstenite
- **前端**：React 18 + TypeScript + Ant Design 5 + Vite（浅/暗双主题）
- **ASR**：百炼异步文件转写 API（提交 → 轮询 → 下载）+ 实时 WebSocket 流式识别
- **翻译**：OpenAI 兼容 Chat Completions API
- **音频**：ffmpeg（文件提取）、ScreenCaptureKit + cpal（实时采集）、rubato（重采样）

## 许可证

[FSL-1.1-ALv2](LICENSE) — 非商用免费使用，商用需授权；发布两年后自动转为 Apache 2.0。
