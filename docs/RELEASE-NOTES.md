# dotOrcA v1.1.0

- 专注硬件编码：移除训练数据库、性能预测及自学习；硬编失败直接报告，不自动软编。
- 简体中文 / English、入队即读取源媒体信息、双击任务打开非模态详情、取消项目弱化显示；配置及窗口位置记忆。
- HEVC/AV1 默认 10-bit，H264 保留源位深；NVENC 根据实际驱动 Caps 启用 B 帧、参考帧、Lookahead 和 Temporal AQ。
- 短片首轮视频预算 95%、长片 98%；NVENC 短片峰值 1.2×，长片 1.4×。保留硬容量上限及超限重试。修复 NV12/P010 硬解后 BWDIF 2× 格式协商。
- 600 秒分界 BPP/FastStart、AAC copy 与紧预算降码率、HEVC hvc1、实际倍速、低 BPP 提示。
- 每个程序会话一份日志，后台串行写入，稳定任务 ID 区分，默认保留最近 30 份本项目日志；详情使用同一任务快照，不反复读取磁盘。
- 正式 ZIP 仅 dotorca.exe、README.md；完整许可证合并到 README，SHA256 单独提供。CI 保留构建证据及诊断工具。

实机验收：候选 a4ea478，用户以 HEVC、1280 长边、68% 目标测试 6 个 SDR/PQ/HLG/隔行样本，6 成功、0 失败、0 重试。容量均未超限。会话日志候选 b22defd 通过 Windows 自动回归，用户确认实机所有项目成功并授权发布。QSV/AMF、真实 UNC、干净 Windows 仍待专项实测。

解压到可写目录，自备支持本机硬件的 ffmpeg.exe / ffprobe.exe。详见 README.md。项目自有代码 MIT，第三方许可保持不变。

测试范围说明：Windows 为唯一发行平台。新增 NV12/P010 模拟硬解输入专项在 Windows CI 通过；Ubuntu runner 的 FFmpeg 在该合成 swscale 图中出现堆内存损坏，尚未解决，因此此专项限定 Windows。Linux 原有隔行编码与其他流水线回归保留。
