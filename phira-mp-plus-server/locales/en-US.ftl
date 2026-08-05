
# ── Room Management ──

create-id-occupied = Room ID is occupied
join-game-ongoing = Game is ongoing
join-room-full = Room is full
join-room-locked = Room is locked
start-no-chart-selected = No chart selected
only-host-can-do = Only the host can do this
already-in-room = You're already in a room
room-not-found = Room not found
already-ready = You're already ready
not-ready = You're not ready yet
already-uploaded = You've already submitted your record
aborted = You've aborted the game
invalid-record = Invalid record record
repeated-authenticate = Repeated authentication request

# ── Session / Auth ──

auth-invalid-token = Invalid authentication token
auth-server-unreachable = Authentication server unreachable, please try again later
auth-banned = You have been banned from this server. Reason: { $reason }
auth-banned-default-reason = Violation of server rules
auth-banned-ip-reason = IP address is banned
auth-cache-hit = Authentication cache hit for user { $user_id }
reconnect = Reconnecting...
no-room = Not in a room
invalid-state = Invalid room state

# ── CLI Messages ──

cli-plugin-not-found = Plugin '{ $name }' not found
cli-room-not-found = Room '{ $name }' not found
cli-user-not-found = User #{ $id } not found
cli-invalid-args = Invalid arguments. Usage: { $usage }
cli-command-not-found = Unknown command: { $name }
cli-plugin-enabled = Plugin '{ $name }' enabled
cli-plugin-disabled = Plugin '{ $name }' disabled
cli-plugin-reloaded = All plugins reloaded ({ $count } loaded)

# ── Server Messages ──

server-shutting-down = Server is shutting down...
server-started = Server started on port { $port } (HTTP port { $http_port })
server-stats = Users: { $users } | Rooms: { $rooms } | Sessions: { $sessions } | Plugins: { $plugins }
join-room-banned = You are banned from this room

join-game-ongoing-warning = This room is in-game. Confirm again to join.
server-room-limit-reached = Server room limit reached (max { $limit })
room-creation-disabled = Room creation is currently disabled
admin-start-in-progress = Administrative start is already in progress
chat-disabled = Chat is disabled
already-authenticated = Already authenticated

# ── System messages (broadcast to clients) ──

user-became-host = { $name } became the host
host-transferred-to = Host transferred to { $name }
host-set-to-system = Host has been set to system
admin-started-game = The server has started the game. Please load the chart and ready up
game-start-failed-retry = Failed to start the game. Please ready up again to retry
result-summary = Results: { $passed }/{ $total } completed
result-ranking-title = ▸ { $chart_name } Results
result-player-line = #{ $rank }. { $name }  { $score }pts  Acc { $accuracy }%  ±{ $std }ms{ $fc }{ $status }
result-detail-line =     Perfect:{ $perfect }  Good:{ $good }  Bad:{ $bad }  Miss:{ $miss }  MaxCombo:{ $max_combo }
result-aborted = DNF
result-fc = FC
kicked-by-admin = You have been kicked by the admin: { $reason }
room-closed-by-admin = Room has been closed by admin
user-kicked-from-room = User { $name } has been kicked from the room
user-moved-to-room = User { $name } has been moved to this room by admin
system-broadcast-prefix = [Broadcast]

# ── Welcome message ──

welcome-message = Welcome [user_name] to HSN Phira-mp+! Currently [player-count] online. Rooms starting with - are hidden, you can join in-progress rooms! Check out https://phira.htadiy.com/ for more features. Join our QQ group 1049578201!\nYou have played for [playtime] on this server\n--------------------------------------------------\nPlaytime leaderboard: [top_playtime]\n--------------------------------------------------\nActive rooms: [active_rooms]
welcome-no-rooms = No active rooms
welcome-locked = locked
welcome-cycling = cycling
welcome-room-line = { $room }{ $flags }(Host:{ $host } [{ $players }/{ $max }])
welcome-playtime-value = { $hours }h
welcome-rank-line = #{ $rank } { $name }  { $hours }h

# ── Chart info ──

chart-info-line = Charter:{ $charter }    Composer:{ $composer }    Difficulty: { $level }{ $rating }{ $updated }
chart-rating =     Rating: { $rating }
chart-updated =     Chart updated: { $date }

# ── Room ban notice ──

room-ban-notice = You have been banned from this room
room-ban-notice-reason = You have been banned from this room: { $reason }

# ── Phira retry notice ──

phira-retry-notice = Phira server connection is unstable, retrying for a smooth experience
