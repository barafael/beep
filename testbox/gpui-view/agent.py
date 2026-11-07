#!/usr/bin/env python3
"""Talks to b33p's agent harness socket (target/gpui-view/run/agent.sock): each argument is a
JSON request without its id, e.g.
  agent.py '{"method":"act","action":{"type":"select","conversation":"c:alice"}}'
Prints each answer. `snapshot` answers are summarised unless --full is the first argument."""
import json, os, socket, sys

here = os.path.dirname(os.path.abspath(__file__))
path = os.path.join(here, "../../target/gpui-view/run/agent.sock")
args = sys.argv[1:]
full = args[:1] == ["--full"]
if full:
    args = args[1:]
with socket.socket(socket.AF_UNIX) as s:
    s.connect(path)
    f = s.makefile("rw")
    for n, request in enumerate(args, 1):
        call = json.loads(request)
        call["id"] = n
        f.write(json.dumps(call) + "\n")
        f.flush()
        answer = json.loads(f.readline())
        result = answer.get("result")
        if not full and isinstance(result, dict) and "conversations" in result:
            result = {k: result[k] for k in ("screen", "selected", "draft", "error") if k in result} | {
                "chats": [c["id"] for c in result["conversations"]],
                "messages": len(result["messages"])}
        print(json.dumps(result if "error" not in answer else {"error": answer["error"]}))
