# 快速开始

## 安装方式

### 方式一：APT 包管理器（推荐）

```bash
sudo add-apt-repository ppa:fireflyf09/phira-mp-plus
sudo apt update
sudo apt install phira-mp-plus-server
```

或手动添加源：
```bash
echo "deb http://ppa.launchpad.net/fireflyf09/phira-mp-plus/ubuntu $(lsb_release -cs) main" | sudo tee /etc/apt/sources.list.d/phira-mp-plus.list
sudo apt-key adv --keyserver keyserver.ubuntu.com --recv-keys 1B92CD21EAC1F4C1401F1F25360EE8752C12B3B1
sudo apt update
sudo apt install phira-mp-plus-server
```

### 方式二：源码构建

前置条件：
- Rust 工具链（版本见 `rust-toolchain.toml`）
- （可选）PostgreSQL 14+

## 构建

```bash
# 构建（默认 features：postgres + wit-bindgen）
cargo build --release --locked

# 仅游戏运行（无数据库、无 WASM 插件）
cargo build --release --locked --no-default-features

# 仅 PostgreSQL 持久化（无 WASM）
cargo build --release --locked --no-default-features --features postgres

# 仅 WASM 插件（无 PostgreSQL）
cargo build --release --locked --no-default-features --features wit-bindgen
```

## 运行

```bash
# 使用默认配置（监听 12346 TCP + 127.0.0.1:12347 HTTP）
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
curl http://127.0.0.1:12347/api/events
```
