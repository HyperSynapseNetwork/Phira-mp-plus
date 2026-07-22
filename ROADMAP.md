# Phira-mp+ Roadmap

## v0.4.x — 预生产加固 ✅

- WAL crash recovery 与 ACK 可靠性 ✅
- 房间状态 Actor ownership ✅（全部 17 个命令走 actor_state）
- Telemetry 持久化与 barrier 确认 ✅
- 插件 TCP 连接 API ✅
- RBAC 删除（两级权限够用）✅
- Federation TLS 删除（保留纯 TCP）✅

## v0.5 — 生产就绪

- [ ] 完整 Readiness 检查（WAL/Supervisor/插件状态）
- [ ] 标准化 /metrics（Prometheus）
- [ ] Backup/Restore 增强（自动 restore、加密）
- [ ] PPB→PMP 服务认证
- [ ] 文档冻结

## 未来

- [ ] Room 降级为纯广播接口（ActorState 完全所有）
- [ ] Federation 连接配额管理
- [ ] 插件动态配置热加载
