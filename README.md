# dotOrcA 🫍

Windows x64 全自动视频压缩器，Rust + CUI v1.5.0。指定每个文件的容量上限，自动计算码率，仅使用硬件编码。

## 使用方法

1. 从 GitHub Releases 下载 Windows x64 ZIP，解压到可写目录。
2. 在 dotorca.exe 同级的 tools/ 目录放入兼容的 FFmpeg 和 FFprobe（ffmpeg.exe、ffprobe.exe；需支持本机硬件编码器）。也可通过标题栏右键高级设置指定 FFmpeg 与输出目录，自动模式先找 tools/ 再找系统 PATH 中同目录的工具组。发行包不包含 FFmpeg。
3. 拖入视频，设置固定 MiB 或原文件百分比、H.264/HEVC/AV1、最大边，点击 Start queued。
4. 成品保存在 output/，日志位于 logs/（每次程序会话一份，默认保留最近 30 份 dotOrcA 日志）。Reset 后可修改参数再次编码，已有成品不会被覆盖。

源文件已在容量上限以内则跳过；源尺寸未超过最大边则保留原分辨率。PQ/HLG 保留位深与色彩，隔行源逐场反交错。已知双 NVENC 型号最多并行两任务。硬编失败直接报告，不自动切换软编。输出按参数命名，例如 Movie_50%_HEVC_1920.MP4，同名直接跳过。

## 已知边界

v1.1.0 移除性能预测、训练数据与自学习。支持简体中文/英文切换，语言、容量与编码参数、目录及主/详情窗口位置保存在 config/；点击任务详情打开独立只读窗口。QSV/AMF、真实 UNC 和无开发工具的干净 Windows 环境仍待专项验证。兼容性以实际 FFmpeg 和 GPU 能力为准；已识别的动态 HDR 不支持时明确失败。

## 开发

纯 Rust：cargo test --locked、cargo check --locked。GUI 需要 Windows MSVC、CMake 与固定 Rust 工具链；这些不是运行时依赖。常规 CI 单入口，手动运行执行全套回归。项目自有代码采用 [MIT License](LICENSE)，Copyright (c) 2026 rampageX。第三方代码、资源（包括 Twemoji 虎鲸图标）及外置 FFmpeg 保留各自的许可；正式发行包的完整许可文本附于 README.md 末尾；CI 产物中保留独立许可和构建证据。

[项目章程](docs/CHARTER.md) · [总任务卡](docs/ROADMAP.md) · [架构](docs/ARCHITECTURE.md) · [工程规范](AGENTS.md) · [发行说明](docs/RELEASE-NOTES.md)
