# 拾言 (Shiyane)

把视频变成带译文的 SRT/VTT 字幕，或把系统声音/麦克风变成实时字幕。本地桌面应用，支持 macOS 与 Windows。

[English](README.en.md) · [下载最新版](https://github.com/usongon/shiyane/releases/latest)

## 预览

**文件转字幕**

<img src="docs/images/home-file-subtitle.png" width="720" alt="文件转字幕首页">

**实时字幕 · 音源选择**

<img src="docs/images/realtime-source-select.png" width="720" alt="实时字幕音源选择">

**实时字幕 · 原文与译文**

<img src="docs/images/realtime-translation.png" width="720" alt="实时字幕翻译效果">

**悬浮字幕 · 置顶字幕条**

<img src="docs/images/realtime-overlay.png" width="600" alt="桌面悬浮字幕">

## 功能

- **文件转字幕** — 拖入视频（mp4、mkv、avi、mov 等），选源语言（或自动识别），导出包含原文与译文的 SRT/VTT。单视频最长 3 小时。逐句显示进度；崩溃或重启后从断点继续，不重复上传与转写；随时暂停，停止则清零重跑。最近处理过的视频与状态列在主页，可继续未完成的任务。
- **实时字幕** — 监听系统音频（可只选某个应用）或麦克风，实时输出原文与译文；支持暂停/恢复/停止。
- **悬浮字幕窗** — 独立的置顶悬浮字幕条，看片/开会无需切换窗口；背景不透明度可调。
- **密钥本地存储** — API Key 加密存储在本地（AES-256-GCM + Argon2），只发送到对应 API，不经过任何第三方。
- **音频即用即删** — 音频经签名 URL 上传到你自己的 OSS Bucket，转写完成后自动删除。

## 安装

### macOS

从 [Releases](https://github.com/usongon/shiyane/releases) 下载 `.dmg`，拖入 Applications 即可使用。ffmpeg/ffprobe 已内置，无需安装任何命令行工具。

### Windows

从 [Releases](https://github.com/usongon/shiyane/releases) 下载 `.exe` 安装包（NSIS，当前用户安装，无需管理员）。系统要求 Windows 11 x64。

- 首次运行 SmartScreen 提示：点「更多信息」→「仍要运行」。安装包未做代码签名，可对照 release 公布的 sha256 校验。
- 已知限制：DRM 内容与个别应用（如 Teams 会议）无法按进程捕获音频，此时音源选择「系统音频（全部）」即可。

## 需要准备

识别与翻译在云端完成，使用前需准备：

1. [百炼](https://bailian.console.aliyun.com/) API Key（北京区域）— 语音识别
2. 阿里云 [OSS](https://oss.console.aliyun.com/) Bucket（私有即可）及具有读写权限的 RAM AccessKey — 音频中转，用后即删
3. 一个翻译 API Key — OpenAI / 百炼 / DeepSeek / Kimi 任选，其他 OpenAI 兼容服务也可以

## 配置

打开应用的**设置**页，按组填入：

| 组 | 字段 | 说明 |
|----|------|------|
| 语音识别 | API Key / 业务空间 ID | 百炼（北京区域）；Workspace ID 在控制台右上角获取 |
| | 文件转写模型 | 默认 `qwen-audio-3.0-asr-flash-filetrans` |
| | 实时识别模型 | 默认 `qwen-audio-3.0-asr-flash-streaming` |
| 翻译 | 渠道 / API Key | OpenAI / 百炼 / DeepSeek / Kimi，模型按渠道自动填充，可改 |
| | 目标语言 | 中文 / 英文 / 日文 / 韩文 |
| 对象存储 | Endpoint / Bucket / AK | 例如 `oss-cn-hangzhou.aliyuncs.com` |
| | 路径前缀 | 可选，例如 `shiyane-temp/` |

语音识别与翻译配置在保存前可测试连通性。配置文件与加密密钥存储在本地：

- macOS：`~/Library/Application Support/com.usongon.shiyane/`
- Windows：`%APPDATA%\com.usongon.shiyane\`

## 从源码构建

```bash
# Rust（2024 edition）+ Node.js ≥ 20 + Tauri CLI
cargo install tauri-cli

git clone https://github.com/usongon/shiyane.git
cd shiyane
cargo tauri build    # 自动安装前端依赖并构建，产物在 target/release/bundle/
```

- macOS 可选运行 `./scripts/build-ffmpeg.sh` 生成捆绑的 ffmpeg/ffprobe（arm64，LGPL）；不运行则使用系统 PATH 中的 ffmpeg。
- Windows 需在 Windows 11 + VS Build Tools 环境构建，ffmpeg sidecar 用 `scripts/fetch-ffmpeg-windows.sh` 拉取。

第三方组件许可见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。

## 开发

```bash
cargo tauri dev                        # Vite dev server + Rust 增量编译 + 热更新
cd src-ui && npm install && npm run dev   # 仅前端，浏览器演示模式（模拟数据，无需密钥）
```

## 项目结构

```
src/               # Rust 后端：ASR（文件/实时）、音频采集、OSS、翻译、
                   # 流水线、断点续传、字幕生成、配置加密、Tauri 命令
src-ui/            # 前端（React + TypeScript + Ant Design）：
                   # 文件转字幕 / 实时字幕 / 悬浮窗 / 设置视图，
                   # Tauri 桥接层 + 浏览器 mock 演示层
```

## 许可证

[FSL-1.1-ALv2](LICENSE) — 个人使用、内部商用、教育与科研免费；将本软件用于竞争性产品或服务需获得授权。每个版本发布满两年后自动转为 Apache 2.0。
