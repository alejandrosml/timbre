#!/usr/bin/env python3
"""End-to-end check of the MCP server over stdio, on the synthetic store.

Usage: python3 -I tests/e2e_mcp.py target/debug/timbre
"""
import json, os, subprocess, sys, tempfile, time

binary = os.path.abspath(sys.argv[1])
root = tempfile.mkdtemp(prefix="timbre-e2e-")
subprocess.run([binary, "dev-fixture", root], check=True)
env = dict(os.environ,
           TIMBRE_MAIL_ROOT=f"{root}/Library/Mail",
           TIMBRE_ACCOUNTS_DB=f"{root}/Library/Accounts/Accounts4.sqlite",
           TIMBRE_DATA_DIR=f"{root}/data",
           TIMBRE_ATTACHMENTS_DIR=f"{root}/attachments")
p = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, text=True)
nid = 0
def rpc(method, params=None, notify=False):
    global nid
    msg = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        msg["params"] = params
    if not notify:
        nid += 1
        msg["id"] = nid
    p.stdin.write(json.dumps(msg) + "\n"); p.stdin.flush()
    if notify:
        return None
    while True:
        line = p.stdout.readline()
        if not line:
            raise SystemExit("server closed: " + p.stderr.read())
        m = json.loads(line)
        if m.get("id") == nid:
            if "error" in m:
                raise SystemExit(f"{method} failed: {m['error']}")
            return m["result"]

init = rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}})
print("server:", init["serverInfo"]["name"], init["serverInfo"]["version"], "| protocol", init["protocolVersion"])
assert "untrusted" in init.get("instructions", "")
rpc("notifications/initialized", notify=True)
tools = rpc("tools/list")["tools"]
names = [t["name"] for t in tools]
print("tools:", len(names), names)
assert "prepare_send" not in names, "sending must be off by default"
ro = {t["name"] for t in tools if (t.get("annotations") or {}).get("readOnlyHint")}
print("read-only:", sorted(ro))
assert {"search_mail", "read_mail", "get_thread", "list_mailboxes", "read_attachment", "mail_status"} <= ro
assert "move_mail" not in ro and tools[names.index("move_mail")]["annotations"]["destructiveHint"] is True
size = len(json.dumps(tools))
print(f"tools/list payload: {size} chars (~{size//4} tokens)")

def call(name, args):
    r = rpc("tools/call", {"name": name, "arguments": args})
    if r.get("isError"):
        raise SystemExit(f"{name} error: {r['content']}")
    return r.get("structuredContent") or json.loads(r["content"][0]["text"])

for _ in range(50):  # wait for the background index
    st = call("mail_status", {})
    if st["index"]["state"] == "ready":
        break
    time.sleep(0.2)
print("status:", st["messages"], "messages, index", st["index"]["state"], st["index"]["indexed_messages"])
assert st["index"]["state"] == "ready"
r = call("search_mail", {"query": "facture"})
print("search facture:", [(h["id"], h["subject"]) for h in r["results"]], r["engine"])
assert [h["id"] for h in r["results"]] == [101]
m = call("read_mail", {"id": 101})
assert "Ignore all previous" not in m["body"] and m["hidden_text_removed"] > 0
print("read 101: body ok, hidden removed:", m["hidden_text_removed"], "| notice:", m["notice"][:40], "…")
m = call("read_mail", {"id": 105})
print("read 105 warnings:", m["warnings"])
assert any("DMARC" in w for w in m["warnings"])
r = rpc("tools/call", {"name": "search_mail", "arguments": {"mailbox": "nowhere"}})
assert r.get("isError") and "list_mailboxes" in r["content"][0]["text"]
print("errors guide the model: ok")
p.stdin.close(); p.wait(timeout=5)
print("E2E OK")
