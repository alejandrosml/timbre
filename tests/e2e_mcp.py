#!/usr/bin/env python3
"""End-to-end check of the MCP server over stdio, on the synthetic store.

Usage: python3 -I tests/e2e_mcp.py target/debug/timbre
"""
import json, os, subprocess, sys, tempfile, time

binary = os.path.abspath(sys.argv[1])
root = tempfile.mkdtemp(prefix="timbre-e2e-")
subprocess.run([binary, "dev-fixture", root], check=True)
def store_env(root, **extra):
    return dict(os.environ,
                TIMBRE_MAIL_ROOT=f"{root}/Library/Mail",
                TIMBRE_ACCOUNTS_DB=f"{root}/Library/Accounts/Accounts4.sqlite",
                TIMBRE_DATA_DIR=f"{root}/data",
                TIMBRE_ATTACHMENTS_DIR=f"{root}/attachments",
                TIMBRE_CHECK_UPDATES="0",  # no network in tests
                **extra)
env = store_env(root)
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

# Clients such as Claude Desktop reject a result that does not match the
# tool's advertised output schema: check every result the same way.
schemas = {t["name"]: t.get("outputSchema") for t in tools}

def conforms(schema, value, defs, path="$"):
    if schema is None or schema is True:
        return
    if "$ref" in schema:
        name = schema["$ref"].rsplit("/", 1)[-1]
        return conforms(defs[name], value, defs, path)
    for key in ("anyOf", "oneOf"):
        if key in schema:
            errors = []
            for option in schema[key]:
                try:
                    conforms(option, value, defs, path)
                    break
                except AssertionError as e:
                    errors.append(str(e))
            else:
                raise AssertionError(f"{path}: matches no option: {errors}")
    types = schema.get("type")
    if types is not None:
        types = types if isinstance(types, list) else [types]
        kinds = {"null": value is None, "boolean": isinstance(value, bool),
                 "integer": isinstance(value, int) and not isinstance(value, bool),
                 "number": isinstance(value, (int, float)) and not isinstance(value, bool),
                 "string": isinstance(value, str), "array": isinstance(value, list),
                 "object": isinstance(value, dict)}
        assert any(kinds[t] for t in types), f"{path}: {value!r} is not {types}"
    if "enum" in schema:
        assert value in schema["enum"], f"{path}: {value!r} not in {schema['enum']}"
    if isinstance(value, dict):
        for key in schema.get("required", []):
            assert key in value, f"{path}: missing required property {key!r}"
        for key, sub in (schema.get("properties") or {}).items():
            if key in value:
                conforms(sub, value[key], defs, f"{path}.{key}")
    if isinstance(value, list) and "items" in schema:
        for i, item in enumerate(value):
            conforms(schema["items"], item, defs, f"{path}[{i}]")

checked = set()
def call(name, args):
    r = rpc("tools/call", {"name": name, "arguments": args})
    if r.get("isError"):
        raise SystemExit(f"{name} error: {r['content']}")
    out = r.get("structuredContent")
    if schemas.get(name):
        assert out is not None, f"{name}: structured content missing"
        try:
            conforms(schemas[name], out, schemas[name].get("$defs", {}))
        except AssertionError as e:
            raise SystemExit(f"{name}: result does not match its output schema: {e}")
        checked.add(name)
    return out or json.loads(r["content"][0]["text"])

for _ in range(50):  # wait for the background index
    st = call("mail_status", {})
    if st["index"]["state"] == "ready":
        break
    time.sleep(0.2)
print("status:", st["messages"], "messages, index", st["index"]["state"], st["index"]["indexed_messages"])
assert st["index"]["state"] == "ready"
if sys.platform == "darwin":
    # macOS must check Timbre's own Full Disk Access, not its launcher's.
    print("permissions checked for:", st.get("permissions_checked_for"))
    assert st.get("permissions_checked_for") == "timbre (this program)"
r = call("search_mail", {"query": "facture"})
print("search facture:", [(h["id"], h["subject"]) for h in r["results"]], r["engine"])
assert [h["id"] for h in r["results"]] == [101]
m = call("read_mail", {"id": 101})
assert "Ignore all previous" not in m["body"] and m["hidden_text_removed"] > 0
print("read 101: body ok, hidden removed:", m["hidden_text_removed"], "| notice:", m["notice"][:40], "…")
m = call("read_mail", {"id": 105})
print("read 105 warnings:", m["warnings"])
assert any("DMARC" in w for w in m["warnings"])
# Results with optional parts left out (no attachment, read, not flagged…).
m = call("read_mail", {"id": 102})
call("search_mail", {"query": "project", "group_threads": False})
call("search_mail", {"unread": False, "sort": "newest", "limit": 50})
call("get_thread", {"id": 101})
call("list_mailboxes", {})
att = call("read_attachment", {"id": 101, "index": 0})
assert "42,50" in att["text"], att
print("results match their output schemas:", sorted(checked))
r = rpc("tools/call", {"name": "search_mail", "arguments": {"mailbox": "nowhere"}})
assert r.get("isError") and "list_mailboxes" in r["content"][0]["text"]
print("errors guide the model: ok")

# New mail is indexed within seconds (the server watches Mail's database).
import sqlite3
v10 = f"{root}/Library/Mail/V10"
mail = sqlite3.connect(f"{v10}/MailData/Envelope Index")
inbox_url = next(u for (u,) in mail.execute("SELECT url FROM mailboxes") if u.endswith("/INBOX"))
inbox = mail.execute("SELECT ROWID FROM mailboxes WHERE url = ?", (inbox_url,)).fetchone()[0]
account = inbox_url.split("/")[2]
store_dir = next(os.scandir(f"{v10}/{account}/INBOX.mbox")).path  # the store UUID folder
raw = "From: Zed <zed@example.com>\r\nSubject: Quokka\r\nMessage-ID: <q3001@example.com>\r\n\r\nquokka report\r\n"
# Mail numbers new messages above every existing one; 3001 lives in Data/3/Messages.
os.makedirs(f"{store_dir}/Data/3/Messages", exist_ok=True)
with open(f"{store_dir}/Data/3/Messages/3001.emlx", "w") as f:
    f.write(f"{len(raw)}\n{raw}<?xml version=\"1.0\"?><plist version=\"1.0\"><dict/></plist>\n")
mail.execute("INSERT INTO subjects(subject) VALUES('Quokka')")
subj = mail.execute("SELECT ROWID FROM subjects WHERE subject = 'Quokka'").fetchone()[0]
mail.execute("INSERT INTO messages(ROWID, sender, subject_prefix, subject, date_sent, date_received, mailbox, flags, read, flagged, deleted, size, conversation_id) "
             "VALUES(3001, 1, '', ?, 1793100000, 1793100000, ?, 0, 0, 0, 0, 100, 3001)", (subj, inbox))
mail.commit(); mail.close()
start = time.time()
while time.time() - start < 20:
    if [h["id"] for h in call("search_mail", {"query": "quokka"})["results"]] == [3001]:
        break
    time.sleep(0.25)
lag = time.time() - start
print(f"new mail searchable after {lag:.1f} s")
assert lag < 6, "new mail should be indexed within a few seconds"
p.stdin.close()
assert p.wait(timeout=5) == 0, "closing standard input stops the server cleanly"

if sys.platform == "darwin":
    # The server runs in a relaunched child. If Claude stops the launcher
    # without closing standard input, the child must not linger.
    q = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env)
    child = None
    for _ in range(50):
        out = subprocess.run(["pgrep", "-P", str(q.pid)], capture_output=True, text=True).stdout.split()
        if out:
            child = int(out[0]); break
        time.sleep(0.1)
    assert child, "the launcher relaunches the server as its child"
    q.terminate(); q.wait(timeout=5)
    for _ in range(50):
        try:
            os.kill(child, 0)
        except ProcessLookupError:
            break
        time.sleep(0.1)
    else:
        raise SystemExit("the server outlived its launcher")
    print("server exits with its launcher: ok")

# Before Mail's data is readable: the same tools, each explaining what is
# missing; once it is, Timbre restarts by itself within the same session.
class Client:
    def __init__(self, env):
        self.log = tempfile.TemporaryFile(mode="w+")
        self.p = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.log,
                                  env=env, text=True)
        self.n = 1000

    def rpc(self, method, params=None, notify=False):
        msg = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = params
        if not notify:
            self.n += 1
            msg["id"] = self.n
        self.p.stdin.write(json.dumps(msg) + "\n"); self.p.stdin.flush()
        if notify:
            return None
        while True:
            line = self.p.stdout.readline()
            if not line:
                self.log.seek(0)
                raise SystemExit("server closed: " + self.log.read())
            m = json.loads(line)
            if m.get("id") == self.n:
                if "error" in m:
                    raise SystemExit(f"{method} failed: {m['error']}")
                return m["result"]

late = tempfile.mkdtemp(prefix="timbre-e2e-late-")
# Without access: a Mail folder this user cannot read (root reads anything,
# so there the folder is missing instead, as when Mail was never set up).
locked = os.geteuid() != 0
if locked:
    os.makedirs(f"{late}/Library/Mail")
    os.chmod(f"{late}/Library/Mail", 0)
c = Client(store_env(late, TIMBRE_LAUNCHER="1"))
c.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}})
c.rpc("notifications/initialized", notify=True)
waiting_tools = [t["name"] for t in c.rpc("tools/list")["tools"]]
assert waiting_tools == names, (waiting_tools, names)
r = c.rpc("tools/call", {"name": "search_mail", "arguments": {"query": "facture"}})
text = r["content"][0]["text"]
assert r.get("isError"), r
if locked:
    assert "open_permission_settings" in text and "Full Disk Access" in text, text
else:
    assert "Apple Mail has no data on this Mac yet" in text, text
# A tool that is not in the list is refused, as by the full server.
m = {"jsonrpc": "2.0", "id": 99999, "method": "tools/call", "params": {"name": "no_such_tool", "arguments": {}}}
c.p.stdin.write(json.dumps(m) + "\n"); c.p.stdin.flush()
while True:
    reply = json.loads(c.p.stdout.readline())
    if reply.get("id") == 99999:
        break
assert "error" in reply, reply
print("before access: same tools, each says what to do")
if locked:
    os.chmod(f"{late}/Library/Mail", 0o755)
    os.rmdir(f"{late}/Library/Mail")
subprocess.run([binary, "dev-fixture", late], check=True)
start = time.time()
while True:
    r = c.rpc("tools/call", {"name": "search_mail", "arguments": {"query": "facture"}})
    if not r.get("isError") and (r.get("structuredContent") or {}).get("results"):
        break
    assert time.time() - start < 30, f"Timbre did not start working by itself: {r}"
    time.sleep(0.5)
print(f"after access: working {time.time() - start:.1f} s later, in the same session")
assert [h["id"] for h in r["structuredContent"]["results"]] == [101]
assert [t["name"] for t in c.rpc("tools/list")["tools"]] == names
c.p.stdin.close()
assert c.p.wait(timeout=10) == 0
print("E2E OK")
