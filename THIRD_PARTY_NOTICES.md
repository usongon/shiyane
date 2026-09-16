# 第三方组件 / Third-party components

## FFmpeg (ffmpeg, ffprobe)

拾言在 macOS 发行包中捆绑了 FFmpeg 的 `ffmpeg` 与 `ffprobe` 可执行文件（子进程调用），
使用户无需另行安装即可使用。

Shiyane bundles the FFmpeg `ffmpeg` and `ffprobe` executables in its macOS release
(invoked as separate processes), so users do not need to install FFmpeg themselves.

| | |
|---|---|
| 版本 / Version | FFmpeg 6.0 |
| 许可证 / License | LGPL v2.1 或更高（构建时禁用 GPL 与非自由组件：`--disable-gpl --disable-nonfree`） |
| 源码 / Source | https://ffmpeg.org/releases/ffmpeg-6.0.tar.xz |
| 许可证全文 / License text | https://www.gnu.org/licenses/old-licenses/lgpl-2.1.html |
| 构建脚本 / Build script | `scripts/build-ffmpeg.sh`（可复现构建，未做任何修改） |

捆绑的二进制为**未修改的原始构建产物**，位于应用包内 `Shiyane.app/Contents/MacOS/`，
可被同版本二进制直接替换。

The bundled binaries are **unmodified upstream builds** located in
`Shiyane.app/Contents/MacOS/` and may be replaced with equivalent builds of the
same version.

### SHA-256（aarch64-apple-darwin）

```
0ffabcfc5e26ceae660b914798480487029ea217f608e7e1cffd470e427e28de  ffmpeg
f0bfd4a806af0ebd5a3f80c7d02d02048d5c075109f34bbcc713a4561434ce0c  ffprobe
```
