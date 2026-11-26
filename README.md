# dotOrcA 🐋

全自动视频压缩器。以目标文件容量为硬约束，在画质与处理时间之间智能决策。

技术栈：Rust + CUI v1.5.0（CPU 软件渲染），Windows x64，便携式单 EXE，外置编码器和数据。核心与 Benchmark 保持纯 Rust；CUI 通过薄 C ABI 桥接。S08 候选实现已接入生产编码与批量队列，验收状态见 [S08.md](docs/S08.md)。

- [项目章程](docs/CHARTER.md)
- [总任务卡](docs/ROADMAP.md)
- [架构](docs/ARCHITECTURE.md)
- [技术选型与验收记录](docs/S00.md)
- [工程规范](AGENTS.md)

纯 Rust：在根目录执行 `cargo test --locked`、`cargo check --locked`。GUI 仅在 Windows MSVC 构建；CMake 先构建 CUI 静态库，Cargo 再链接，具体命令见 CI。构建需要 Rust/CMake/MSVC，不是用户运行时依赖。许可证待定，暂不发布正式版。
