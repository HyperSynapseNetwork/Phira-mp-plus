# Capability 映射表

> 自动生成。每项 Capability 对应一组 WIT 方法，主机根据插件的 manifest 授予。

| Capability | 覆盖方法 | 默认可用 |
|---|---|---|
| `state.read` | phira-query.`get-user`, phira-query.`get-room`, phira-query.`list-rooms`, phira-query.`list-online-users`, phira-query.`is-user-online`, phira-persistence.`query-events`, phira-persistence.`query-room-snapshots`, phira-persistence.`query-touches`, phira-persistence.`query-judges`, phira-persistence.`get-playtime`, phira-persistence.`top-playtime`, phira-runtime.`status`, phira-runtime.`events`, phira-runtime.`commands` | ✅ |
| `send` | phira-host.`send-chat`, phira-messaging.`send-to-user`, phira-messaging.`send-to-room`, phira-messaging.`send-to-all` | ✅ |
| `ext` | phira-query.`get-user-extra`, phira-query.`set-user-extra`, phira-query.`get-room-extra` | ✅ |
| `config` | phira-config.`get-config`, phira-config.`set-config`, phira-config.`list-config`, phira-config.`reload-config`, phira-config.`poll-config-changes` | ✅ |
| `file.read` | （无） | ✅ |
| `file.write` | （无） | ✅ |
| `plugin.call` | （无） | ✅ |
| `plugin.register` | （无） | ✅ |
| `http` | phira-host.`http-request` | ❌ 需 manifest |
| `room.manage` | phira-room-mgmt.`create-empty-room`, phira-room-mgmt.`kick-from-room`, phira-room-mgmt.`transfer-host`, phira-room-mgmt.`set-host`, phira-room-mgmt.`set-room-lock`, phira-room-mgmt.`set-room-hidden`, phira-room-mgmt.`close-room`, phira-room-mgmt.`set-room-phira-api-endpoint` | ❌ 需 manifest |
| `admin` | phira-user-mgmt.`kick-user`, phira-user-mgmt.`ban-user`, phira-user-mgmt.`unban-user`, phira-user-mgmt.`get-ban-list`, phira-user-mgmt.`is-banned`, phira-admin.`list-admin-ids`, phira-admin.`is-admin`, phira-admin.`add-admin-id`, phira-admin.`remove-admin-id`, phira-admin.`set-admin-ids` | ❌ 需 manifest |
| `simulation` | phira-simulation.`status`, phira-simulation.`run`, phira-simulation.`stop`, phira-simulation.`cleanup` | ❌ 需 manifest |
| `crypto` | phira-crypto.`sign`, phira-crypto.`verify`, phira-crypto.`sha256` | ❌ 需 manifest |
| `timer` | （无） | ❌ 需 manifest |
| `tcp` | phira-tcp.`connect`, phira-tcp.`listen`, phira-tcp.`send`, phira-tcp.`close` | ❌ 需 manifest |

