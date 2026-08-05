
# ── 房間管理 ──

create-id-occupied = 房間 ID 已被佔用
join-game-ongoing = 遊戲正在進行中
join-room-full = 房間已滿
join-room-locked = 房間已鎖定
start-no-chart-selected = 還沒有選擇譜面
only-host-can-do = 只有房主才能執行此操作
already-in-room = 你已經在房間中了
room-not-found = 房間不存在
already-ready = 你已準備
not-ready = 你還沒準備
already-uploaded = 你已經上傳過成績了
aborted = 你已中止遊戲
invalid-record = 無效的成績記錄
repeated-authenticate = 重複的認證請求

# ── 工作階段/驗證 ──

auth-invalid-token = 無效的認證令牌
auth-server-unreachable = 認證伺服器不可達，請稍後重試
auth-banned = 你已被此伺服器封禁。原因：{ $reason }
auth-banned-default-reason = 違反伺服器規則
auth-banned-ip-reason = IP 位址已被封禁
auth-cache-hit = 認證快取命中，用戶 { $user_id }
reconnect = 重新連線中...
no-room = 不在房間中
invalid-state = 無效的房間狀態

# ── CLI 訊息 ──

cli-plugin-not-found = 插件 '{ $name }' 未找到
cli-room-not-found = 房間 '{ $name }' 未找到
cli-user-not-found = 用戶 #{ $id } 未找到
cli-invalid-args = 無效參數。用法：{ $usage }
cli-command-not-found = 未知命令：{ $name }
cli-plugin-enabled = 插件 '{ $name }' 已啟用
cli-plugin-disabled = 插件 '{ $name }' 已停用
cli-plugin-reloaded = 所有插件已重新載入（已載入 { $count } 個）

# ── 伺服器訊息 ──

server-shutting-down = 伺服器正在關閉...
server-started = 伺服器已在連接埠 { $port } 啟動（HTTP 連接埠 { $http_port }）
server-stats = 用戶：{ $users } | 房間：{ $rooms } | 會話：{ $sessions } | 插件：{ $plugins }
join-room-banned = 你已被此房間封禁

join-game-ongoing-warning = 該房間遊戲進行中，請再次確認以加入
server-room-limit-reached = 伺服器房間數已達上限（最多 { $limit }）
room-creation-disabled = 暫不允許玩家建房
admin-start-in-progress = 管理員發起遊戲正在進行中
chat-disabled = 聊天功能已停用
already-authenticated = 已經認證過了

# ── 系統訊息（廣播給客戶端） ──

user-became-host = { $name } 成為了房主
host-transferred-to = 房主已轉移給 { $name }
host-set-to-system = 房主已設為系統？
admin-started-game = 伺服器已發起遊戲，請加載譜面並點擊準備
game-start-failed-retry = 遊戲啟動失敗，請重新點擊準備重試
result-summary = 完成率：{ $passed }/{ $total } 已完成
result-ranking-title = ▸ { $chart_name } 排行
result-player-line = #{ $rank }. { $name }  { $score }分  準確率 { $accuracy }%  ±{ $std }ms{ $fc }{ $status }
result-detail-line =     Perfect:{ $perfect }  Good:{ $good }  Bad:{ $bad }  Miss:{ $miss }  MaxCombo:{ $max_combo }
result-aborted = 放棄
result-fc = FC
kicked-by-admin = 你已被管理員踢出伺服器：{ $reason }
room-closed-by-admin = 房間已被管理員關閉
user-kicked-from-room = 用戶 { $name } 已被管理員踢出房間
user-moved-to-room = 用戶 { $name } 已被管理員強制轉移到本房間
system-broadcast-prefix = [系統廣播]

# ── 歡迎語 ──

welcome-message = 歡迎 [user_name] 來到 HSN Phira-mp+！當前在線 [player-count] 人。以-開頭的房间會被隱藏，可以進入遊戲中的房間哦。可以前往 https://phira.htadiy.com/ 使用更多相關功能哦。也歡迎加入我們的QQ交流群1049578201！\n您在本伺服器上游玩了[playtime]\n--------------------------------------------------\n遊玩時間排行榜：[top_playtime]\n--------------------------------------------------\n活躍房間：[active_rooms]
welcome-no-rooms = 暫無房間
welcome-locked = 鎖定
welcome-cycling = 循環
welcome-room-line = { $room }{ $flags }(房主:{ $host } [{ $players }/{ $max }])
welcome-playtime-value = { $hours }h
welcome-rank-line = #{ $rank } { $name }  { $hours }h

# ── 譜面資訊 ──

chart-info-line = 譜師:{ $charter }    曲師:{ $composer }    難度: { $level }{ $rating }{ $updated }
chart-rating =     評分: { $rating }
chart-updated =     譜面更新: { $date }

# ── 房間黑名單通知 ──

room-ban-notice = 你已被加入此房間的黑名單
room-ban-notice-reason = 你已被加入此房間的黑名單：{ $reason }

# ── Phira 重試通知 ──

phira-retry-notice = Phira伺服器連線不穩定，正在重試以確保你的流暢體驗
