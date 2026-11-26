# 架构 V1.1

Cargo Workspace，四个 crate：core（纯 Rust）、benchmark（独立 CLI，依赖 core）、gui（Rust UI adapter + C bridge）、s00-probe（独立测试计时器，非生产引擎）。默认成员不含 GUI；无需 CMake 或 C 编译器即可编译/测试核心与 Benchmark。S00 只有边界和探针，尚不实现 S01+。

拟议 core 模块（不创建空壳层级）：media/hardware/process（S01）；benchmark 标准记录及执行（S02）；predict（S03）；budget（S04）；decision（S05）；pipeline（S06）；dataset（S07）。平台专属 Windows 进程管理和硬件接口放在明确的平台边界。GUI 在 S08 接入公开 core API，不直接依赖编码器。GUI 只消费不可变进度快照和提交请求。

数据契约拟议：MediaInfo 包含路径（PathBuf）、时长、宽高、精确有理帧率、编码格式、位深、HDR 类型及颜色标签；未知属性用 Option；JobRequest 包含容量 u64 bytes、输出编码、最大分辨率；Estimate 包含可选耗时/容量、置信度与证据 ID；Decision 包含路线及原因码；BenchmarkRecord 包含 schema 版本、素材哈希、CPU/GPU/驱动、编码器/版本/预设、输入属性、帧数、实际耗时/容量及有版本的实测质量指标。实际类型在对应阶段落实；不提前冻结未验证接口。UI 容量单位与批量每文件/合计规则待明确。

便携布局：dotOrcA.exe、tools/、config/、data/official/、data/local/、logs/、work/；以 EXE 目录为基准。官方和本地数据独立版本与更新。用户配置、输出和临时文件分离。大文件用流式处理，不整片载入内存。

GUI：CUI v1.5.0 静态 C 库，MSVC static CRT，C ABI bridge 负责组件绘制及事件，Rust 主导任务状态；CPU renderer 强制使用，空闲按事件唤醒。可能仍导入系统 opengl32.dll（上游同时编译两种 renderer）；不等于创建 OpenGL 上下文。中文字体使用系统 fallback，不分发字体文件；首次加载成本需记录。长期维护风险：上游年轻、缺完整 accessibility，混合 DPI/IME/路径仍需实际验证。锁定版本，更新需独立回归。
