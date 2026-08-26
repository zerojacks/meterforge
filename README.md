# MeterForge — DL/T 645-2007 虚拟电表模拟器

**版本**: 0.1.0
**仓库**: <https://github.com/zerojacks/meterforge>
**技术栈**: Rust（nightly） · Tokio · GPUI + gpui-component · SQLite (sqlx)

---

## 📋 项目概述

高性能的 DL/T 645-2007 电能表协议模拟器与监控平台：

- **完整协议支持**：帧编解码（+33H 偏移 / 校验和）、全部控制码（11H–1BH）、DI 数据项读写
- **真实仿真行为**：虚拟时钟、脉冲累加、最大需量滑差、事件检测（失压/欠压/过压/断相/过流等）、冻结调度、负荷记录、费率时段表
- **多通道接入**：串口（RS485 总线仿真）、TCP 服务器 / TCP 客户端
- **多表并发**：Actor 架构 + `MeterRegistry`，目标规模 2000 表
- **图形化监控**：GPUI 界面 — 表列表、实时数据、历史曲线、参数管理、通信日志面板
- **数据持久化**：SQLite 存储，支持断电恢复与优雅关闭

---

## 🏗️ 项目结构

```
meter_engine/
├── Cargo.toml                  # Workspace 配置
├── rust-toolchain.toml         # 固定 nightly 工具链
├── .github/workflows/release.yml   # CI 发布（tag 触发）
│
├── meter-core/                 # 核心虚拟表引擎（无 UI 依赖，可独立测试）
│   ├── src/
│   │   ├── protocol/           # 协议层：帧结构、编解码、控制码
│   │   ├── simulation/         # 仿真层：物理引擎、电表状态、DI 数据项处理
│   │   ├── actor/              # 多表并发：MeterActor + MeterRegistry
│   │   ├── transport/          # 传输层：串口 / TCP 通道
│   │   ├── router.rs           # 帧路由（按通信地址分发到虚拟表）
│   │   ├── connection.rs       # 连接管理器（串口 / TCP server / TCP client）
│   │   ├── communication_log.rs# 通信报文日志
│   │   ├── snapshot.rs         # 状态快照
│   │   ├── persistence/        # SQLite 持久化 worker
│   │   └── error.rs
│   ├── migrations/             # 数据库迁移脚本
│   ├── tests/                  # 集成测试（端到端 / 传输 / 持久化 / 恢复 / 优雅关闭）
│   └── examples/               # 示例程序
│
├── meter-ui/                   # GPUI 图形界面
│   ├── src/
│   │   ├── main.rs             # 应用入口
│   │   ├── backend/            # 后端引导与命令通道
│   │   ├── pages/              # 页面：表列表 / 详情 / 实时数据 / 历史 / 参数 / 通信日志
│   │   ├── components/         # 通用组件（标题栏、表卡片）
│   │   ├── settings/           # 连接配置、仿真配置、参数下发对话框
│   │   ├── state.rs / types.rs # 状态管理与类型
│   │   └── assets.rs
│   ├── assets/icon/            # 应用图标（ico / icns / png）
│   └── build.rs                # Windows 图标内嵌 + 主线程栈配置
│
├── packaging/                  # 各平台打包脚本与资源（见其 README）
├── data/                       # 运行时 SQLite 数据库（meters.db）
└── docs/                       # 设计文档
```

---

## 🚀 快速开始

### 前置要求

- **Rust**：仓库的 `rust-toolchain.toml` 已固定 nightly，rustup 会在构建时自动安装，无需手动切换默认工具链。
- **Linux** 需要安装 GPUI 的系统依赖：

  ```bash
  sudo apt-get install -y \
    pkg-config libudev-dev \
    libx11-dev libxcb1-dev libxcb-render0-dev libxcb-shape0-dev \
    libxcb-xfixes0-dev libxcb-icccm4-dev libxcb-keysyms1-dev \
    libxcb-randr0-dev libxcb-util-dev libxkbcommon-dev libxkbcommon-x11-dev \
    libwayland-dev libvulkan-dev \
    libfontconfig1-dev libfreetype-dev
  ```

- **SQLite**：由 sqlx 静态捆绑，无需系统安装。

### 编译运行

```bash
git clone https://github.com/zerojacks/meterforge.git
cd meterforge

# 启动图形界面
cargo run -p meter-ui

# 测试核心引擎（108 个单元测试 + 5 组集成测试）
cargo test -p meter-core
```

> 报文解析库 `dlt645-2007` 来自 [protocol-parser](https://github.com/zerojacks/protocol-parser) 仓库，
> 以 git 依赖引入，cargo 会自动拉取，克隆本项目后无需任何本地准备。

### 构建行为说明

- **debug 构建**：Windows 下保留控制台窗口，tracing 日志直接输出到终端，便于开发调试。
- **release 构建**：Windows 下为 GUI 子系统（`windows_subsystem = "windows"`），双击启动不弹
  控制台窗口；应用图标已由 `meter-ui/build.rs` 内嵌到 exe 资源。

---

## 📦 打包与发布

### 本地打包

macOS 生成 `.app`、Linux 安装 desktop 图标等，见 [packaging/README.md](packaging/README.md)。

### CI 发布（GitHub Actions）

`.github/workflows/release.yml` 在推送 `v*` tag 时触发，四个环境并行编译 release，
自动按约定式提交归类生成 changelog，并创建 GitHub Release 挂载产物：

Release 同时发布自定义格式的 `latest.json`，客户端可通过
`https://github.com/zerojacks/meterforge/releases/latest/download/latest.json`
检查新版本及对应平台安装包，并使用其中的 SHA-256 值校验下载文件。

| 平台 | 产物 |
|------|------|
| Windows x86_64 | `MeterForge-Setup-<版本>.exe`（安装包）和 `MeterForge-windows-x86_64-<版本>.zip`（便携版） |
| Linux x86_64 | `MeterForge-linux-x86_64-<版本>.deb`（安装包）和 `MeterForge-linux-x86_64-<版本>.tar.gz`（便携版） |
| macOS x86_64 | `MeterForge-darwin-x86_64-<版本>.pkg`（安装包）和 `.dmg`（拖拽安装） |
| macOS aarch64 | `MeterForge-darwin-aarch64-<版本>.pkg`（安装包）和 `.dmg`（拖拽安装） |

发布新版本：

```bash
git tag v0.2.0
git push origin v0.2.0
```

tag 中的版本号会写入 `Cargo.toml` 与 macOS `Info.plist`。

---

## 📖 设计文档

1. [虚拟645电表模拟器_设计方案.md](docs/虚拟645电表模拟器_设计方案.md) — 总体架构、协议要点、仿真算法、数据库设计
2. [电表物理模型_模拟方案.md](docs/电表物理模型_模拟方案.md) — 电气量物理模型
3. [simulation_algorithms.md](docs/simulation_algorithms.md) — 仿真算法细节
4. [UI集成设计方案.md](docs/UI集成设计方案.md) — UI 架构与组件设计

---

## 📝 提交规范

CI 的 changelog 按约定式提交自动归类，请遵循：

```
feat: 添加新功能
fix: 修复bug
docs: 文档更新
test: 测试相关
refactor: 代码重构
perf: 性能优化
```

---

## 📄 许可证

MIT OR Apache-2.0

---

## 🙏 致谢

- [GPUI](https://github.com/zed-industries/zed) — 高性能 UI 框架
- [gpui-component](https://github.com/longbridge/gpui-component) — UI 组件库
- [protocol-parser](https://github.com/zerojacks/protocol-parser) — 多协议报文解析库
- [Tokio](https://tokio.rs/) — 异步运行时
- [SQLx](https://github.com/launchbadge/sqlx) — 异步数据库驱动
