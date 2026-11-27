# 架构 V1.2（v1.1.0）

Cargo Workspace：core（纯 Rust）、gui（Rust UI adapter + C bridge）、s00-probe（仅开发测试）。核心无需 CUI/CMake/C 编译器。生产模块为 media/hardware/process、budget、decision、pipeline、queue；text 集中管理应用文本。S02/S03/S07 的 Benchmark、预测、训练数据及学习代码已移除，历史记录保留。

数据契约：MediaInfo 保留原始媒体属性；JobRequest 使用 u64 字节预算；Decision 选择兼容硬件路线，失败明确报告。队列提供稳定 ID 与不可变快照，生产流水线不依赖 GUI，保留双任务并行、封装容量硬上限与超限重试、分辨率及 HDR 策略。无预测或额外试编码。

便携布局：dotorca.exe、tools/、config/、output/、logs/、work/；以 EXE 目录为基准。语言、主窗口及详情窗口位置大小、容量/编码/分辨率及自定义输出与工具目录保存在 config/；自定义 FFmpeg 目录优先，否则 tools/ 后查找系统 PATH 中同目录的 FFmpeg/FFprobe；任务启动时冻结目录。旧 data/ 不再读取或写入，不主动删除用户文件。

GUI：CUI v1.5.0 固定上游 commit，MSVC static CRT，软件渲染。主窗口与独立详情窗口均在 UI 线程以各自 CUI 上下文处理，不嵌套模态循环；一个详情窗口可切换任务，通过稳定 ID 获取最新快照，任务移除或主窗口退出时销毁。五个只读分区支持选择、复制、滚动，错误和标题高亮。C ABI 传递 UTF-8；中文使用系统字体 fallback，不分发字体。默认随系统语言，用户可切换并记忆设置。原始 FFmpeg 诊断保留原文。混合 DPI 和无 CJK 字体环境仍需实机验证。

长视频策略：>600s 启用 BPP 自适应，≤600s FastStart；AAC 已兼容低码率音轨可 Copy，其余保留声道按预算编码，必要时降低 AAC 码率；NVENC P5/HQ/QuarterRes/Spatial AQ/lookahead16 为候选测试参数；HEVC hvc1、自动硬解、实际倍速与低 BPP 提示。HEVC/AV1 默认 10-bit；H264 保留源位深。NVENC Windows x64 直接动态加载系统驱动，查询 SDK 12.1 Caps，不编码任何帧；多 GPU 对同编码格式取支持能力交集，未知时禁用可选增强。B 帧最多 3、Middle reference/Temporal AQ/Lookahead 仅能力确认后启用。

入队后的单一后台元数据线程读取文件大小及 FFprobe 容器/流信息；只读预览通过稳定 ID 发布，取消/移除/重置不串任务，不等待预览即可开始编码。生产源探测仍为权威数据。主列表显示首个非封面视频摘要，详情显示容器和音视频/字幕流；取消文件名弱化，双击任务卡打开详情。

日志：Queue 的有界通道接收终态任务快照（包括未开始即取消的任务），独立单一写入者在首次记录时创建会话文件。任务 ID 分隔记录；GUI 从同源内存快照提取详情，无磁盘解析。默认保留最近 30 个已识别本项目日志（含旧格式）；无关文件不删除，写入/轮换错误显示于状态栏。正式 ZIP 仅 EXE 与附完整许可的 README，构建证据留 CI。


v1.1.1 配置布局：config/settings.json 包含 encoding、directories、main_window、details_window 四个命名对象；ui-language.txt 保持独立。旧配置全部验证并原子写入后清理，局部保存保留其他字段；无新增运行时依赖。
