#!/usr/bin/env bash
set -euo pipefail

# docgen.sh -- WIT -> Markdown API documentation generator
#
# Parses wit/phira-plugin.wit, maps each method to its required capability
# (by cross-referencing the Rust host source), and generates:
#   docs/api/plugin-api.md            -- Interface/method reference
#   docs/api/capability-table.md      -- Capability -> covered methods table
#
# Run from the repo root:
#   bash scripts/docgen.sh

# ---------------------------------------------------------------------------
# Locate repo root
# ---------------------------------------------------------------------------
if GIT_ROOT=$(git rev-parse --show-toplevel 2>/dev/null); then
  cd "$GIT_ROOT"
else
  cd "$(dirname "${BASH_SOURCE[0]}")/.."
fi

WIT="wit/phira-plugin.wit"
OUT_DIR="docs/api"
OUT_API="${OUT_DIR}/plugin-api.md"
OUT_CAP="${OUT_DIR}/capability-table.md"

if [ ! -f "$WIT" ]; then
  echo "ERROR: $WIT not found." >&2
  exit 1
fi

mkdir -p "$OUT_DIR"

# Write JSON summary to a temp file to avoid bash variable expansion issues
TMP_JSON=$(mktemp)
trap 'rm -f "$TMP_JSON"' EXIT

python3 - "$WIT" > "$TMP_JSON" << 'PYEOF'
import json, re, sys

wit_path = sys.argv[1]
with open(wit_path) as wf:
    wit = wf.read()
interfaces = []
in_iface = False
brace_depth = 0
iface_name = ""
iface_doc = ""
iface_body = ""
doc_buf = ""

for line in wit.split("\n"):
    stripped = line.strip()

    if not in_iface and stripped.startswith("///"):
        doc_buf += stripped.lstrip("/").strip() + " "
        continue

    m = re.match(r'\s*interface\s+([a-zA-Z0-9_-]+)\s*\{', line)
    if m and not in_iface:
        iface_name = m.group(1)
        iface_doc = doc_buf.strip()
        doc_buf = ""
        in_iface = True
        brace_depth = 1
        iface_body = ""
        continue

    if in_iface:
        iface_body += line + "\n"
        brace_depth += line.count("{") - line.count("}")
        if brace_depth <= 0:
            interfaces.append({
                "interface": iface_name,
                "doc": iface_doc,
                "body": iface_body
            })
            in_iface = False
            iface_name = ""
            iface_doc = ""
            iface_body = ""

    if not in_iface and not stripped.startswith("///") and stripped:
        doc_buf = ""

def extract_methods(body):
    methods = []
    for line in body.split("\n"):
        ls = line.strip()
        if not ls or ls.startswith("use ") or ls.startswith("record ") or ls.startswith("variant ") or ls == "}":
            continue
        rm = re.match(r'\s*([a-zA-Z0-9_-]+)\s*:\s*func\s*\(([^)]*)\)\s*(->\s*(.*))?', line)
        if rm:
            mname = rm.group(1)
            params = rm.group(2).strip()
            ret = (rm.group(4) or "").strip().rstrip(";").strip()
            methods.append({"name": mname, "params": params, "return": ret})
    return methods

for iface in interfaces:
    iface["methods"] = extract_methods(iface["body"])

exports = []
in_world = False
brace_depth = 0
for line in wit.split("\n"):
    if re.match(r'\s*world\s+phira-plugin-v2', line):
        in_world = True
        brace_depth = 1
        continue
    if not in_world:
        continue
    brace_depth += line.count("{") - line.count("}")
    if brace_depth <= 0:
        break
    rm = re.match(r'\s*export\s+([a-zA-Z0-9_-]+)\s*:\s*func\s*\(([^)]*)\)\s*(->\s*(.*))?', line)
    if rm:
        mname = rm.group(1)
        params = rm.group(2).strip()
        ret = (rm.group(4) or "").strip().rstrip(";").strip()
        exports.append({"name": mname, "params": params, "return": ret})

print(json.dumps({"interfaces": interfaces, "exports": exports}))
PYEOF

# ---------------------------------------------------------------------------
# Run Python generator script, passing the JSON file path and output paths
# ---------------------------------------------------------------------------
export TMP_JSON OUT_API OUT_CAP
python3 << 'PYEOF'
import json, os, re

# Read the JSON summary
json_path = os.environ['TMP_JSON']
with open(json_path) as f:
    data = json.load(f)

out_api = os.environ['OUT_API']
out_cap = os.environ['OUT_CAP']

# ------- Capability mapping (same data as bash assoc arrays) -------
cap_map = {}
sq_map = {}

def cap(iface, method, value):
    cap_map[(iface, method)] = value

def sq(iface, method, value):
    sq_map[(iface, method)] = value
    cap_map[(iface, method)] = "state_query"

# phira-host
cap("phira-host","log","none")
cap("phira-host","generate-uuid","none")
cap("phira-host","current-time-ms","none")
cap("phira-host","api-call","none")
cap("phira-host","send-chat","send")
cap("phira-host","http-request","http")

# phira-query
sq("phira-query","get-user","user_name")
cap("phira-query","get-user-extra","ext")
cap("phira-query","set-user-extra","ext")
sq("phira-query","get-room","rooms.by_name")
cap("phira-query","get-room-extra","ext")
sq("phira-query","list-rooms","rooms.list")
sq("phira-query","list-online-users","users.list")
sq("phira-query","is-user-online","user.is_online")

# phira-room-mgmt
sq("phira-room-mgmt","create-empty-room","room.create_empty")
sq("phira-room-mgmt","kick-from-room","room.kick")
sq("phira-room-mgmt","transfer-host","room.set_host")
sq("phira-room-mgmt","set-host","room.set_host")
sq("phira-room-mgmt","set-room-lock","room.set_lock")
sq("phira-room-mgmt","set-room-hidden","room.set_hidden")
sq("phira-room-mgmt","close-room","room.close")
sq("phira-room-mgmt","set-room-phira-api-endpoint","room.set_phira_api_endpoint")

# phira-user-mgmt
sq("phira-user-mgmt","kick-user","user.kick")
sq("phira-user-mgmt","ban-user","ban.add")
sq("phira-user-mgmt","unban-user","ban.remove")
sq("phira-user-mgmt","get-ban-list","ban.list")
sq("phira-user-mgmt","is-banned","ban.check")

# phira-messaging
cap("phira-messaging","send-to-user","send")
sq("phira-messaging","send-to-room","send_room_chat")
cap("phira-messaging","send-to-all","send")

# phira-persistence
sq("phira-persistence","query-events","persist.events")
sq("phira-persistence","query-room-snapshots","persist.rooms")
sq("phira-persistence","query-touches","persist.touches")
sq("phira-persistence","query-judges","persist.judges")
sq("phira-persistence","get-playtime","persist.playtime")
sq("phira-persistence","top-playtime","persist.top_playtime")

# phira-admin
sq("phira-admin","list-admin-ids","admin.list")
sq("phira-admin","is-admin","admin.check")
sq("phira-admin","add-admin-id","admin.add")
sq("phira-admin","remove-admin-id","admin.remove")
sq("phira-admin","set-admin-ids","admin.set")

# phira-config
cap("phira-config","get-config","config")
cap("phira-config","set-config","config")
cap("phira-config","list-config","config")
cap("phira-config","reload-config","config")
cap("phira-config","poll-config-changes","config")

# phira-simulation
cap("phira-simulation","status","simulation")
cap("phira-simulation","run","simulation")
cap("phira-simulation","stop","simulation")
cap("phira-simulation","cleanup","simulation")

# phira-crypto
cap("phira-crypto","sign","crypto")
cap("phira-crypto","verify","crypto")
cap("phira-crypto","sha256","crypto")
cap("phira-crypto","get-node-public-key","none")

# phira-timer
cap("phira-timer","set-timer","none")
cap("phira-timer","clear-timer","none")

# phira-tcp
cap("phira-tcp","connect","tcp")
cap("phira-tcp","listen","tcp")
cap("phira-tcp","send","tcp")
cap("phira-tcp","close","tcp")

# phira-runtime
sq("phira-runtime","status","runtime.status")
sq("phira-runtime","events","runtime.event_stats")
sq("phira-runtime","commands","runtime.commands")

cap("exports","init","none")
cap("exports","get-info","none")
cap("exports","cleanup","none")
cap("exports","on-event","none")
cap("exports","on-api","none")

def resolve_cap(method):
    if method in ("uuid.v4", "time.now"):
        return "none"
    # Exact matches (checked before prefix matches for correctness)
    exact_map = {
        "user.kick": "admin",
        "state.query": "state.read",
        "rooms.list": "state.read", "rooms.by_name": "state.read",
        "rooms.by_user": "state.read", "rooms.history": "state.read",
        "auth.visited_count": "state.read", "user_name": "state.read",
        "users.list": "state.read", "user.is_online": "state.read",
        "playtime.leaderboard": "state.read",
        "send_room_chat": "send",
        "file.read": "file.read", "file.write": "file.write",
        "plugin.api_call": "plugin.call", "plugin.api_register": "plugin.register",
        "room.create_empty": "room.manage", "room.kick": "room.manage",
        "room.set_host": "room.manage", "room.clear_host": "room.manage",
        "room.set_lock": "room.manage", "room.force_move": "room.manage",
        "room.set_hidden": "room.manage", "room.set_persistent_empty": "room.manage",
        "room.set_phira_api_endpoint": "room.manage", "room.clear_phira_api_endpoint": "room.manage",
        "room.close": "room.manage", "room.set_cycle": "room.manage",
    }
    if method in exact_map:
        return exact_map[method]
    # Prefix matches
    for prefix, cap_val in [
        ("admin.", "admin"), ("ban.", "admin"),
        ("room.", "room.manage"),
        ("player.", "state.read"), ("round.", "state.read"),
        ("user.", "state.read"), ("persist.", "state.read"),
        ("runtime.", "state.read"), ("benchmark.", "state.read"),
        ("send.", "send"), ("ext.", "ext"), ("config.", "config"),
        ("http.", "http"), ("sse.", "http"),
        ("simulation.", "simulation"),
        ("crypto.", "crypto"), ("timer.", "timer"), ("tcp.", "tcp"),
    ]:
        if method.startswith(prefix):
            return cap_val
    return "none"

def get_method_cap(iface, mname):
    raw = cap_map.get((iface, mname), "none")
    if raw == "state_query":
        sq_method = sq_map.get((iface, mname), "")
        return resolve_cap(sq_method)
    return raw

default_caps = {"state.read", "send", "ext", "config", "file.read", "file.write", "plugin.call", "plugin.register"}
all_caps = ["state.read", "send", "ext", "config", "file.read", "file.write", "plugin.call", "plugin.register", "http", "room.manage", "admin", "simulation", "crypto", "timer", "tcp"]

IFACE_DESC = {
    "phira-types": "核心数据类型",
    "phira-host": "核心主机 API",
    "phira-events": "事件类型定义",
    "phira-query": "用户/房间数据查询",
    "phira-room-mgmt": "房间管理操作",
    "phira-user-mgmt": "用户管理与封禁",
    "phira-messaging": "消息发送与广播",
    "phira-persistence": "持久化数据查询",
    "phira-admin": "管理员 ID 配置",
    "phira-config": "插件配置管理",
    "phira-simulation": "模拟运行管理",
    "phira-crypto": "密码学操作",
    "phira-timer": "非实时定时器",
    "phira-tcp": "TCP 网络连接",
    "phira-runtime": "运行时诊断",
}

# ============== plugin-api.md ==============
with open(out_api, "w") as f:
    f.write("# Phira-mp+ 插件 API 参考\n\n")
    f.write("> 自动生成自 `wit/phira-plugin.wit`。请勿手动编辑。\n")
    f.write("> 重新生成: `bash scripts/docgen.sh`\n\n")
    f.write("## 接口概览\n\n")
    f.write("| 接口 | 方法数 | 描述 |\n|---|---|---|\n")

    for iface in data["interfaces"]:
        name = iface["interface"]
        mcount = len(iface["methods"])
        desc = IFACE_DESC.get(name, "")
        f.write(f"| {name} | {mcount} | {desc} |\n")

    export_count = len(data["exports"])
    f.write(f"| exports（插件导出） | {export_count} | 插件生命周期与事件回调 |\n")
    f.write("\n")

    # Interface detail sections
    for iface in data["interfaces"]:
        name = iface["interface"]
        doc = iface["doc"]
        methods = iface["methods"]
        body = iface["body"]

        f.write(f"## {name}\n\n")
        if doc:
            f.write(f"{doc}\n\n")

        if not methods:
            f.write("本接口仅定义类型，不包含可调用方法。\n\n")
            for line in body.split("\n"):
                m = re.match(r'\s*record\s+([a-zA-Z0-9_-]+)\s*\{', line)
                if m:
                    f.write(f"### record `{m.group(1)}`\n\n")
            for line in body.split("\n"):
                m = re.match(r'\s*variant\s+([a-zA-Z0-9_-]+)\s*\{', line)
                if m:
                    f.write(f"### variant `{m.group(1)}`\n\n")
            f.write("---\n")
            continue

        for m in methods:
            mname = m["name"]
            params = m["params"]
            ret = m["return"] or "（无）"
            cap_val = get_method_cap(name, mname)
            cap_str = f"`{cap_val}`" if cap_val != "none" else "（无 — 公开 API）"

            f.write(f"### `{mname}`\n\n")
            body_lines = body.split("\n")
            for i, bl in enumerate(body_lines):
                if f"{mname}:" in bl and "func" in bl:
                    if i > 0 and body_lines[i-1].strip().startswith("///"):
                        f.write(f"{body_lines[i-1].strip().lstrip('/').strip()}\n\n")
                    break

            f.write("**参数**:  \n")
            if params:
                # Parse params respecting angle brackets (don't split on commas inside <>)
                param_list = []
                depth = 0
                current = ""
                for ch in params:
                    if ch == '<':
                        depth += 1
                        current += ch
                    elif ch == '>':
                        depth -= 1
                        current += ch
                    elif ch == ',' and depth == 0:
                        param_list.append(current.strip())
                        current = ""
                    else:
                        current += ch
                if current.strip():
                    param_list.append(current.strip())

                for p in param_list:
                    if ":" in p:
                        pname, ptype = p.split(":", 1)
                        f.write(f"- `{pname.strip()}`: `{ptype.strip()}`\n")
                    else:
                        f.write(f"- `{p}`\n")
            else:
                f.write("（无）\n")
            f.write("\n")

            f.write(f"**返回值**: `{ret}`\n\n")
            f.write(f"**所需 Capability**: {cap_str}\n\n")

        f.write("---\n")

    # Exports section
    f.write("\n## exports（插件导出）\n\n")
    f.write("插件必须实现的导出函数（由主机调用）。\n\n")
    for m in data["exports"]:
        mname = m["name"]
        params = m["params"]
        ret = m["return"] or "（无）"
        f.write(f"### `{mname}`\n\n")
        f.write("**参数**:  \n")
        if params:
            param_list = []
            depth = 0
            current = ""
            for ch in params:
                if ch == '<':
                    depth += 1; current += ch
                elif ch == '>':
                    depth -= 1; current += ch
                elif ch == ',' and depth == 0:
                    param_list.append(current.strip()); current = ""
                else:
                    current += ch
            if current.strip():
                param_list.append(current.strip())

            for p in param_list:
                if ":" in p:
                    pname, ptype = p.split(":", 1)
                    f.write(f"- `{pname.strip()}`: `{ptype.strip()}`\n")
                else:
                    f.write(f"- `{p}`\n")
        else:
            f.write("（无）\n")
        f.write("\n")
        f.write(f"**返回值**: `{ret}`\n\n")
        f.write("**所需 Capability**: （无 — 插件自身实现）\n\n")

print(f"Generated: {out_api}")

# ============== capability-table.md ==============
cap_to_methods = {c: [] for c in all_caps}

for iface in data["interfaces"]:
    name = iface["interface"]
    for m in iface["methods"]:
        cap_val = get_method_cap(name, m["name"])
        if cap_val != "none" and cap_val in cap_to_methods:
            cap_to_methods[cap_val].append(f"{name}.`{m['name']}`")

for m in data["exports"]:
    cap_val = get_method_cap("exports", m["name"])
    if cap_val != "none" and cap_val in cap_to_methods:
        cap_to_methods[cap_val].append(f"exports.`{m['name']}`")

with open(out_cap, "w") as f:
    f.write("# Capability 映射表\n\n")
    f.write("> 自动生成。每项 Capability 对应一组 WIT 方法，主机根据插件的 manifest 授予。\n\n")
    f.write("| Capability | 覆盖方法 | 默认可用 |\n|---|---|---|\n")
    for c in all_caps:
        methods_str = ", ".join(cap_to_methods[c]) if cap_to_methods[c] else "（无）"
        default_str = "✅" if c in default_caps else "❌ 需 manifest"
        f.write(f"| `{c}` | {methods_str} | {default_str} |\n")
    f.write("\n")

print(f"Generated: {out_cap}")
PYEOF

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "========================================"
echo " 文档生成完成！"
echo "========================================"
echo "  API 参考:      $OUT_API"
echo "  Capability 表: $OUT_CAP"
echo ""
echo " 若 WIT 文件或能力映射发生变更，请重新运行:"
echo "   bash scripts/docgen.sh"
