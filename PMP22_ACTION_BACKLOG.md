# PMP `(22)` 下一轮执行清单

## 0. CI / PGO

- [ ] 删除 Build workflow 的 PGO 必需 job
- [ ] 删除 Release workflow 的 PGO 必需步骤
- [ ] 取消所有普通产物的 `-pgo` 命名
- [ ] 保留 release LTO 和 `codegen-units = 1`
- [ ] 文档删除“正式版本使用 PGO”的承诺
- [ ] 未来只保留可选 `workflow_dispatch` PGO
- [ ] 可选 PGO 仅支持真实训练过的平台

## 1. 高频 Touch/Judge 数据

- [ ] 明确队列溢出策略
- [ ] 禁止 `send().await` 无限阻塞 Session 热路径
- [ ] 修正 queue-full 日志与真实行为不一致
- [ ] 暂时关闭非事务 COPY，或实现 staging COPY
- [ ] COPY header/items/canonical 写入必须事务原子
- [ ] 防止 header 已存在导致 fallback 跳过不完整事件
- [ ] 指标增加 point received/committed/dropped
- [ ] 正常关闭验证全部 batch flush
- [ ] 数据库短暂失败故障测试
- [ ] COPY 中途失败故障测试

## 2. 游戏 TCP PROXY protocol

- [ ] 删除 v1 重复 `PROXY ` 前缀
- [ ] 不再把 Tokio socket 转 nonblocking std socket做读取
- [ ] 使用纯 async parser
- [ ] 增加头部读取 timeout
- [ ] 限制 v1/v2 最大头长度
- [ ] 保存并回放预读的 Phira payload
- [ ] 删除错误分支中的替代连接和 `expect`
- [ ] trusted proxy CIDR
- [ ] proxy peer 和 forwarded IP 双重限流
- [ ] v1 分段到达集成测试
- [ ] v2 分段到达集成测试
- [ ] no-proxy 集成测试
- [ ] 不受信代理测试
- [ ] IPv4 / IPv6 测试

## 3. Room Actor

- [ ] 初始化参数直接传入 Actor spawn
- [ ] 修复 empty room endpoint 设置时序
- [ ] 实现 persistent_empty Actor 状态和 PostgreSQL
- [ ] 或删除 persistent_empty 假成功 API
- [ ] 首个正常玩家加入空房间成为房主
- [ ] Join actor/connection registry 失败回滚
- [ ] 删除 Leave direct fallback
- [ ] creator_id fallback 删除
- [ ] 业务成员只以 Actor IDs 为准
- [ ] 连接 weak refs 不参与业务判断
- [ ] 并发 join/leave/host 测试

## 4. Benchmark Real

- [ ] 当前 Real 标记为 smoke，直到完整实现
- [ ] N clients
- [ ] N rooms
- [ ] unique token/user ID
- [ ] unique room ID
- [ ] step timeout
- [ ] scenario timeout
- [ ] 有界 response channel
- [ ] 使用 config.duration
- [ ] 使用 config.scenario
- [ ] Gameplay 60/120 Hz Touch/Judge
- [ ] Hot room
- [ ] Slow consumer
- [ ] Reconnect
- [ ] Plugin load
- [ ] Database write
- [ ] Mixed 并发
- [ ] 报告命令数来自 Collector
- [ ] 自托管目标模式
- [ ] 外部目标模式要求预配置 Mock endpoint

## 5. Mock Phira

- [ ] token 生成确定性唯一 user ID
- [ ] delay_ms 生效
- [ ] jitter_ms 生效
- [ ] error_rate 生效
- [ ] timeout_ms 生效
- [ ] seed 生效
- [ ] response_size 生效
- [ ] listen address 生效
- [ ] verbose 生效
- [ ] 并发用户测试
- [ ] 错误和超时场景测试

## 6. Simulation

- [ ] 不再只运行 shadow-world counter
- [ ] 使用生产 Room Actor
- [ ] 使用生产 Plugin dispatch
- [ ] 使用生产 Persistence gateway
- [ ] 只替换 transport 和时间源
- [ ] connection 不再映射 Balanced
- [ ] slow-consumer 不再映射 Balanced
- [ ] reconnect 不再映射 Balanced
- [ ] plugin-load 不再映射 Balanced
- [ ] database-write 不再映射 Balanced
- [ ] hot-room 真实实现
- [ ] mixed 真并发
- [ ] 删除旧 shadow aliases
- [ ] 删除 hybrid benchmark
- [ ] 删除 benchmark token

## 7. PostgreSQL cutover

- [ ] `database_url` 正式配置必填
- [ ] 删除自动用户名/密码猜测
- [ ] 删除 `allow_database_degraded_mode`
- [ ] 删除 `SkippedNoDatabase`
- [ ] 删除可选 DB 分支
- [ ] `EventMirror` 重命名为真实业务概念
- [ ] 删除 `DirectWrite` 迁移命名
- [ ] JSON 数据分类：config/cache/export/business
- [ ] 业务数据 PostgreSQL 唯一权威
- [ ] extensions 删除 async `block_on`
- [ ] `enqueue_or_write_direct` 重命名并改为 `.await`

## 8. 插件

- [ ] 保留全部 WIT API
- [ ] WIT fixture 加载失败必须测试失败
- [ ] Server/WIT/SDK 版本一致性测试
- [ ] 所有 API conformance tests
- [ ] capability 拒绝测试
- [ ] Plugin TCP listener 配额
- [ ] Plugin TCP connection 配额
- [ ] Plugin TCP bandwidth 配额
- [ ] unload 完整清理
- [ ] 按插件 metrics

## 9. 代码整洁

- [ ] 修复简单 Clippy lint
- [ ] 删除 crate 根简单风格 allow
- [ ] 结构型 allow 移到具体模块
- [ ] 每个局部 allow 添加中文原因
- [ ] 拆分仍包含多个领域的大文件
- [ ] 删除 runtime_v2 文档和字段
- [ ] 删除 mirror/cutover 文档
- [ ] 删除 hybrid/token 旧文档
- [ ] 日志说明中文统一
- [ ] 结构化字段名保持英文
