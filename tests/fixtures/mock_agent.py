#!/usr/bin/env python3
"""Minimal ACP agent used by the integration tests.

It exercises the client-side surface of acp-tui: streamed chunks with message IDs,
tool calls with diffs, fs/read_text_file, terminal/create + wait_for_exit,
session/request_permission, plans, usage updates, and cancellation.
"""
import json
import sys
import threading

lock = threading.Lock()
next_id = [1000]
pending = {}


def send(msg):
    with lock:
        sys.stdout.write(json.dumps(msg) + "\n")
        sys.stdout.flush()


def request(method, params):
    next_id[0] += 1
    rid = next_id[0]
    ev = threading.Event()
    pending[rid] = {"event": ev}
    send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
    ev.wait(10)
    return pending.pop(rid).get("response")


def update(sid, upd):
    send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": upd}})


def run_prompt(rid, params):
    sid = params["sessionId"]
    text = "".join(b.get("text", "") for b in params["prompt"] if b.get("type") == "text")
    update(sid, {"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "Planning."}})
    update(sid, {"sessionUpdate": "plan", "entries": [
        {"content": "Read file", "priority": "high", "status": "completed"},
        {"content": "Run command", "priority": "medium", "status": "in_progress"},
    ]})
    read = request("fs/read_text_file", {"sessionId": sid, "path": params.get("_cwd", "") + "Cargo.toml", "line": 1, "limit": 2})
    first_line = (read or {}).get("result", {}).get("content", "").splitlines()[:1]
    update(sid, {"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Edit a.txt", "kind": "edit",
                 "status": "pending", "content": [{"type": "diff", "path": "/tmp/a.txt", "oldText": "a\nb\n", "newText": "a\nc\n"}]})
    perm = request("session/request_permission", {"sessionId": sid, "toolCall": {"toolCallId": "t1", "title": "Edit a.txt"},
                   "options": [{"optionId": "yes", "name": "Allow", "kind": "allow_once"},
                               {"optionId": "no", "name": "Reject", "kind": "reject_once"}]})
    outcome = (perm or {}).get("result", {}).get("outcome", {})
    update(sid, {"sessionUpdate": "tool_call_update", "toolCallId": "t1",
                 "status": "completed" if outcome.get("optionId") == "yes" else "failed"})
    term = request("terminal/create", {"sessionId": sid, "command": "echo", "args": ["from-terminal"]})
    tid = term["result"]["terminalId"]
    update(sid, {"sessionUpdate": "tool_call", "toolCallId": "t2", "title": "echo", "kind": "execute",
                 "status": "in_progress", "content": [{"type": "terminal", "terminalId": tid}]})
    request("terminal/wait_for_exit", {"sessionId": sid, "terminalId": tid})
    out = request("terminal/output", {"sessionId": sid, "terminalId": tid})
    request("terminal/release", {"sessionId": sid, "terminalId": tid})
    update(sid, {"sessionUpdate": "tool_call_update", "toolCallId": "t2", "status": "completed"})
    for part in ["Echo: ", text, " | ", (first_line or [""])[0], " | ", out["result"]["output"].strip()]:
        update(sid, {"sessionUpdate": "agent_message_chunk", "messageId": "m1", "content": {"type": "text", "text": part}})
    update(sid, {"sessionUpdate": "usage_update", "used": 1000, "size": 200000})
    send({"jsonrpc": "2.0", "id": rid, "result": {"stopReason": "end_turn",
          "usage": {"totalTokens": 30, "inputTokens": 20, "outputTokens": 10}}})


for line in sys.stdin:
    msg = json.loads(line)
    if "method" not in msg:
        entry = pending.get(msg.get("id"))
        if entry is not None:
            entry["response"] = msg
            entry["event"].set()
        continue
    method, rid, params = msg["method"], msg.get("id"), msg.get("params") or {}
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": 1,
            "agentInfo": {"name": "mock", "title": "Mock Agent", "version": "1.0"},
            "agentCapabilities": {"loadSession": False, "promptCapabilities": {"embeddedContext": True}},
            "authMethods": []}})
    elif method == "session/new":
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "sessionId": "s1",
            "modes": {"currentModeId": "code", "availableModes": [
                {"id": "code", "name": "Code"}, {"id": "plan", "name": "Plan"}]},
            "configOptions": [{"id": "model", "name": "Model", "category": "model", "type": "select",
                               "currentValue": "m1", "options": [{"value": "m1", "name": "Model One"},
                                                                 {"value": "m2", "name": "Model Two"}]}]}})
        update("s1", {"sessionUpdate": "available_commands_update",
                      "availableCommands": [{"name": "review", "description": "Review code"}]})
    elif method == "session/prompt":
        params["_cwd"] = sys.argv[1] if len(sys.argv) > 1 else ""
        threading.Thread(target=run_prompt, args=(rid, params), daemon=True).start()
    elif method == "session/set_config_option":
        send({"jsonrpc": "2.0", "id": rid, "result": {"configOptions": []}})
    elif method == "session/cancel":
        pass
    elif rid is not None:
        send({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "Method not found"}})
