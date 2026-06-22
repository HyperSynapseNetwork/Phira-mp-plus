# Phira-mp+

> 基于 Phira-mp 二次开发的增强版 Phira 多人游戏服务端

[![License](https://img.shields.io/badge/License-AGPLv3-blue.svg)](LICENSE)

## 简介

**Phira-mp+** 是 [Phira-mp](https://github.com/team-phira/phira-mp) 的增强版本，在原版基础上增加了强大的 WASM 插件系统、基于 WIT 的 API 系统以及便捷的 CLI 管理控制台。

### 核心特性

- **🧩 WASM 插件系统** — 基于 [wasmtime](https://wasmtime.dev/) 运行时，支持加载 WebAssembly 插件，实现动态功能扩展
- **📋 WIT API 规范** — 使用 [WIT](https://component-model.bytecodealliance.org/design/wit.html) 接口定义语言明确定义插件 API，确保接口清晰、类型安全
- **⚡ 高性能** — 继承 Rust 与 WASM 的高性能与高稳定性
- **🖥️ CLI 管理控制台** — 交互式命令行管理界面，方便管理员管理和维护服务器
- **🔌 扩展数据系统** — 支持插件注册自定义用户/房间数据字段，实现数据灵活扩展
- **🛡️ AGPLv3 开源协议** — 保障软件自由与开发者权益

## 技术栈

| 技术 | 用途 | 版本 |
|------|------|------|
| [Rust](https://www.rust-lang.org/) | 主开发语言 | 2024 Edition |
| [Tokio](https://tokio.rs/) | 异步运行时 | 1.49+ |
| [Clap](https://clap.rs/) | CLI 参数解析 | 4.5+ |
| [wasmtime](https://wasmtime.dev/) | WASM 运行时（可选） | 30+ |
| [fluent](https://projectfluent.org/) | 本地化 (i18n) | 0.17 |
| [reqwest](https://docs.rs/reqwest/) | HTTP 客户端 | 0.12 |
| [serde](https://serde.rs/) | 序列化/反序列化 | 1.0 |
| [tracing](https://docs.rs/tracing/) | 日志与诊断 | 0.1 |
| [WIT](https://component-model.bytecodealliance.org/design/wit.html) | 插件接口定义 | IDL |

## 项目结构

```
Phira-mp-plus/
├── Cargo.toml                     # 工作区根配置
├── Cargo.lock                     # 依赖锁定文件
├── LICENSE                        # AGPLv3 许可证
├── README.md                      # 本文档
│
├── phira-mp-plus-server/          # 增强服务器主程序
│   ├── Cargo.toml                 # 服务器依赖配置
│   ├── locales/                   # 本地化资源
│   │   ├── en-US.ftl              # 英文
│   │   ├── zh-CN.ftl              # 简体中文
│   │   └── zh-TW.ftl              # 繁体中文
│   ├── wit/
│   │   └── phira-mp-plus.wit      # WIT 插件接口规范
│   └── src/
│       ├── main.rs                # 入口点 + CLI 启动参数
│       ├── lib.rs                 # 库导出
│       ├── server.rs              # 增强服务器（连接管理）
│       ├── session.rs             # 会话管理（认证、命令分发、插件事件）
│       ├── room.rs                # 房间管理（状态机、生命周期）
│       ├── cli.rs                 # 交互式管理控制台
│       ├── plugin.rs              # WASM 插件系统（加载/调度/生命周期）
│       ├── extensions.rs          # 扩展数据系统（用户/房间额外字段）
│       └── l10n.rs                # 本地化（Fluent i18n）
│
├── phira-mp-plus-sdk/             # 插件开发工具包 (SDK)
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs                 # SDK 类型定义与插件特征
│
├── plugins/                       # 插件存放目录
│   └── .gitkeep
│
├── docs/
│   ├── cli.md                     # CLI 命令文档
│   └── plugin-dev.md              # 插件开发文档
│
└── phira-mp/                      # 上游 Phira-mp 源码（引用子模块）
```

## 各系统/组件/模块介绍

### 服务器核心 (`phira-mp-plus-server/src/server.rs`)

增强的服务器实例管理，在原始 Phira-mp 基础上增加了：
- 插件管理器集成
- 扩展数据系统
- 事件分发到插件

### 会话管理 (`session.rs`)

管理用户认证、连接生命周期、命令处理。在原始功能基础上，在以下节点触发插件事件：
- 用户连接/断开
- 房间创建/加入/离开
- 房间数据修改（锁定/循环切换）
- 游戏开始/结束

### 房间管理 (`room.rs`)

房间状态机管理（选曲 → 准备 → 游戏进行中），支持最大 8 名玩家，支持旁观者模式。

### WASM 插件系统 (`plugin.rs`)

基于 wasmtime 的可选插件系统：
- **插件发现**: 扫描 `plugins/` 目录下的 `.wasm` 文件
- **生命周期管理**: `init` → `event` → `cleanup`
- **事件驱动**: 插件注册事件监听，服务器触发事件时分发到所有已启用插件
- **原生插件**: 支持 Rust 原生插件开发（NativePlugin trait），方便测试和开发

### 扩展数据系统 (`extensions.rs`)

插件可以注册自定义的数据字段，附加到用户或房间上：
- 用户扩展数据（如黑白名单状态、积分等）
- 房间扩展数据（如房间标签、自定义规则等）
- 其他插件和 CLI 可通过注册的字段名查询数据
- 可选持久化到 JSON 文件

### CLI 管理控制台 (`cli.rs`)

交互式命令行界面，支持：
- 插件管理（列表、启用、禁用、重载）
- 用户/房间管理
- 消息广播
- 服务器状态查看

## 部署教程

### 前置要求

- Rust 工具链（1.85+，推荐 1.96+）
- C 编译器（用于编译依赖）
- libssl-dev（用于 HTTPS 支持）

### 快速开始

```bash
# 1. 克隆仓库
git clone https://github.com/your-org/Phira-mp-plus.git
cd Phira-mp-plus

# 2. 构建（默认包含插件系统）
cargo build --release

# 3. 运行服务器
./target/release/phira-mp-plus-server --port 12346

# 4. （可选）创建插件目录
mkdir -p plugins
```

### 命令行参数

```bash
phira-mp-plus-server [OPTIONS]

选项:
  -p, --port <PORT>          服务器监听端口 [默认: 12346]
  -d, --plugins-dir <DIR>    WASM 插件目录路径 [默认: "plugins"]
  -e, --ext-file <FILE>      扩展数据持久化 JSON 文件路径
      --no-cli               禁用交互式 CLI 管理控制台
  -l, --log-file <NAME>      日志文件基础名称 [默认: "phira-mp-plus"]
  -m, --monitor <IDS>...     允许旁观的用户 ID（可多次指定）
  -h, --help                 显示帮助信息
  -V, --version              显示版本号
```

### Docker 部署（待实现）

```bash
# 构建 Docker 镜像
docker build -t phira-mp-plus .

# 运行容器
docker run -d \
  --name phira-mp-plus \
  -p 12346:12346 \
  -v $(pwd)/plugins:/app/plugins \
  -v $(pwd)/data:/app/data \
  phira-mp-plus
```

### 配置文件

服务器启动时会读取 `server_config.yml`（如果存在）：

```yaml
monitors:
  - 2           # 允许旁观的用户 ID 列表
```

## 插件开发

详细的插件开发文档请参考 [docs/plugin-dev.md](docs/plugin-dev.md)。

### 快速示例

```rust
use phira_mp_plus_sdk::*;

struct MyPlugin;

impl PhiraPlugin for MyPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: "hello-world".into(),
            version: "0.1.0".into(),
            author: "me".into(),
            description: "我的第一个插件".into(),
        }
    }

    fn on_connect(&self, ctx: &PluginContext, user_id: i32, user_name: &str) {
        ctx.log("info", &format!("欢迎！{}({})", user_name, user_id));
    }
}

// 注册为原生插件
// phira_mp_plus_server::plugin::create_native_plugin(MyPlugin)
```

## CLI 命令

详细的 CLI 命令文档请参考 [docs/cli.md](docs/cli.md)。

## TODO / 路线图

### v0.1.0 (当前)
- [x] 基础服务器功能（基于 Phira-mp）
- [x] WASM 插件系统框架
- [x] WIT API 接口规范
- [x] 基础 CLI 管理控制台
- [x] 扩展数据系统
- [x] 插件 SDK

### v0.2.0 (规划)
- [ ] wasmtime 运行时完整集成与 WIT 绑定生成
- [ ] 插件热加载与热更新
- [ ] 完整 CLI 命令（用户/房间管理）
- [ ] Docker 镜像与 CI/CD 流水线
- [ ] 示例插件（白名单、统计、Webhook）

### v0.3.0 (规划)
- [ ] Web 管理面板
- [ ] 插件市场与远程安装
- [ ] 数据库持久化（SQLite/PostgreSQL）
- [ ] 指标收集与监控集成（Prometheus）
- [ ] 性能基准测试与优化

### v0.4.0 (远期)
- [ ] 分布式多节点支持
- [ ] 插件沙箱安全增强
- [ ] 自定义游戏模式支持

## 许可证与开发者义务

本项目基于 **GNU Affero General Public License v3.0 (AGPLv3)** 开源。

### 开发者义务

1. **开源要求**: 任何基于本项目的修改或衍生作品，若通过网络提供服务，必须提供完整的源代码。
2. **版权声明**: 修改后的作品必须保留原始版权声明和许可证信息。
3. **修改记录**: 分发修改版本时，必须明确说明所做的修改。
4. **相同的许可证**: 衍生作品必须同样使用 AGPLv3 许可证。
5. **无附加限制**: 不得对 AGPLv3 许可证授予的权利施加额外的限制。

### 引用与致谢

- [Phira-mp](https://github.com/team-phira/phira-mp) — 上游多人游戏服务端
- [Phira](https://phira.5wyxi.com/) — Phira 社区
- [wasmtime](https://wasmtime.dev/) — WebAssembly 运行时
- [Bytecode Alliance](https://bytecodealliance.org/) — WIT / Component Model 规范

---

**Phira-mp+** — 让 Phira 多人游戏更强大、更灵活。
