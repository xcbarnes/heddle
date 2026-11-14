"""A minimal MCP server over stdio, for the day-0 tests.

  python3 -I day0/mcp-echo.py FILE

One tool, `lookup`, takes no arguments and returns the text of FILE. Enough
of the protocol for Claude Code to list and call it; nothing else.
"""

import json
import sys

path = sys.argv[1]


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    try:
        req = json.loads(line)
    except ValueError:
        continue
    method, rid = req.get("method"), req.get("id")
    if method == "initialize":
        version = req.get("params", {}).get("protocolVersion", "2025-06-18")
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": version,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "day0", "version": "0"},
        }})
    elif method == "tools/list":
        send({"jsonrpc": "2.0", "id": rid, "result": {"tools": [{
            "name": "lookup",
            "description": "Return the record.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False},
        }]}})
    elif method == "tools/call":
        with open(path) as f:
            text = f.read()
        send({"jsonrpc": "2.0", "id": rid, "result": {"content": [{"type": "text", "text": text}]}})
    elif rid is not None:
        send({"jsonrpc": "2.0", "id": rid, "result": {}})
