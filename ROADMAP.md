# Roadmap

## 当前阶段：预生产加固（v0.4.x）

- WAL 终态确认闭环
- Room Actor 全状态所有权
- 数据库版本化迁移
- 内部 HTTP health/readiness/metrics
- PPB→PMP 服务认证

## 下一阶段：单实例生产（v1.0.0）

- 生产 benchmark + soak
- RBAC 接入管理入口
- 结构化指标 + 告警 + runbook
- 备份恢复演练
- 发布签名 + SBOM 强制门禁

## 未来阶段：插件平台（v2.0+）

- 独立插件 worker 进程
- cgroup/seccomp/namespace 隔离
- 插件签名 + manifest + 版本管理
- 运行时热加载 + 回滚

## 远期：高可用（v3.0+）

- 多实例房间调度
- 跨实例会话路由
- 数据库 HA + 跨区域复制
