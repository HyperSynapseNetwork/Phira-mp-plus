# 快速开始

## 安装方式

### 源码构建（推荐）

前置条件：
- Rust 工具链（版本见 `rust-toolchain.toml`）
- （可选）PostgreSQL 14+

```bash
# 克隆仓库
git clone https://github.com/FireflyF09/Phira-mp-plus.git
cd Phira-mp-plus

# 构建（默认 features：postgres + wit-bindgen）
cargo build --release

# 仅游戏运行（无数据库、无 WASM 插件）
cargo build --release --no-default-features

# 仅 PostgreSQL 持久化（无 WASM）
cargo build --release --no-default-features --features postgres

# 仅 WASM 插件（无 PostgreSQL）
cargo build --release --no-default-features --features wit-bindgen
```

### 从 GitHub Releases 下载

前往 [Releases 页面](https://github.com/FireflyF09/Phira-mp-plus/releases) 下载对应架构的预编译二进制。

## 运行

```bash
# 使用默认配置（监听 12346 TCP）
cp server_config.yml data/
./target/release/phira-mp-plus-server

# 指定配置文件和端口
./target/release/phira-mp-plus-server --config /etc/pmp/server_config.yml --port 12346
```

## 配置数据库

编辑 `server_config.yml`：

```yaml
database_url: "postgres://user:password@localhost:5432/phira_mp_plus"
```

也支持环境变量覆盖：

```bash
export PM_DATABASE_URL="postgres://user:password@localhost:5432/phira_mp_plus"
# 或从文件读取
export PM_DATABASE_URL_FILE="/run/secrets/database_url"
```

## 管理命令

```
# 查看帮助
help
help advanced

# 房间管理
room list
room info <id>

# 插件管理
plugin list
plugin reload <name>

# 运行时诊断
runtime status
runtime persistence
```

## 验证运行

```bash
# 检查进程是否运行
ps aux | grep phira-mp-plus-server
```
