
# ── 房间管理 ──

create-id-occupied = 房间 ID 已被占用
join-game-ongoing = 游戏正在进行中
join-room-full = 房间已满
join-room-locked = 房间已锁定
start-no-chart-selected = 还没有选择谱面
only-host-can-do = 只有房主才能执行此操作
already-in-room = 你已经在房间中了
room-not-found = 房间不存在
already-ready = 你已准备
not-ready = 你还没准备
already-uploaded = 你已经上传过成绩了
aborted = 你已中止游戏
invalid-record = 无效的成绩记录
repeated-authenticate = 重复的认证请求

# ── 会话/认证 ──

auth-invalid-token = 无效的认证令牌
auth-server-unreachable = 认证服务器不可达，请稍后重试
auth-banned = 你已被此服务器封禁。原因：{ $reason }
auth-banned-default-reason = 违反服务器规则
auth-cache-hit = 认证缓存命中，用户 { $user_id }
reconnect = 重新连接中...
no-room = 不在房间中
invalid-state = 无效的房间状态

# ── CLI 消息 ──

cli-plugin-not-found = 插件 '{ $name }' 未找到
cli-room-not-found = 房间 '{ $name }' 未找到
cli-user-not-found = 用户 #{ $id } 未找到
cli-invalid-args = 无效参数。用法：{ $usage }
cli-command-not-found = 未知命令：{ $name }
cli-plugin-enabled = 插件 '{ $name }' 已启用
cli-plugin-disabled = 插件 '{ $name }' 已禁用
cli-plugin-reloaded = 所有插件已重新加载（已加载 { $count } 个）

# ── 服务器消息 ──

server-shutting-down = 服务器正在关闭...
server-started = 服务器已在端口 { $port } 启动（HTTP 端口 { $http_port }）
server-stats = 用户：{ $users } | 房间：{ $rooms } | 会话：{ $sessions } | 插件：{ $plugins }
join-room-banned = 你已被此房间封禁

join-game-ongoing-warning = 该房间游戏进行中，请再次确认以加入
server-room-limit-reached = 服务器房间数已达上限（最多 { $limit }）
admin-start-in-progress = 管理员发起游戏正在进行中
chat-disabled = 聊天功能已禁用
already-authenticated = 已经认证过了

# ── 系统消息（广播给客户端） ──

user-became-host = { $name } 成为了房主
host-transferred-to = 房主已转移给 { $name }
host-set-to-system = 房主已设为系统？
admin-started-game = 服务器已发起游戏，请加载谱面并点击准备
result-summary = 完成率：{ $passed }/{ $total } 已完成
kicked-by-admin = 你已被管理员踢出服务器：{ $reason }
room-closed-by-admin = 房间已被管理员关闭
user-kicked-from-room = 用户 { $name } 已被管理员踢出房间
user-moved-to-room = 用户 { $name } 已被管理员强制转移到本房间
system-broadcast-prefix = [系统广播]
