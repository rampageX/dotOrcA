# 架构 V1.2（v1.1.0）

Cargo Workspace：core（纯 Rust）、gui（Rust UI adapter + C bridge）、s00-probe（仅开发测试）。核心无需 CUI/CMake/C 编译器。生产模块为 media/hardware/process、budget、decision、pipeline、queue；text 集中管理应用文本。S02/S03/S07 的 Benchmark、预测、训练数据及学习代码已移除，历史记录保留。

数据契约：MediaInfo 保留原始媒体属性；JobRequest 使用 u64 字节预算；Decision 选择兼容硬件路线，失败明确报告。队列提供稳定 ID 与不可变快照，生产流水线不依赖 GUI，保留双任务并行、封装容量硬上限与超限重试、分辨率及 HDR 策略。无预测或额外试编码。

便携布局：dotorca.exe、tools/、config/、output/、logs/、work/；以 EXE 目录为基准。语言和详情窗口位置大小保存在 config/；旧 data/ 不再读取或写入，不主动删除用户文件。

GUI：CUI v1.5.0 固定上游 commit，MSVC static CRT，软件渲染。主窗口与独立详情窗口均在 UI 线程以各自 CUI 上下文处理，不嵌套模态循环；一个详情窗口可切换任务，通过稳定 ID 获取最新快照，任务移除或主窗口退出时销毁。五个只读分区支持选择、复制、滚动，错误和标题高亮。C ABI 传递 UTF-8；中文使用系统字体 fallback，不分发字体。默认随系统语言，用户可切换并记忆设置。原始 FFmpeg 诊断保留原文。混合 DPI 和无 CJK 字体环境仍需实机验证。
