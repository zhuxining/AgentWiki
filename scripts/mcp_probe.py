#!/usr/bin/env python3
"""AgentWiki MCP stdio 黑盒验收客户端（rmcp 3.3：每行一个 JSON-RPC 消息）。

用法: MCP_CLIENT_HOME=<tmp> python3 mcp_probe.py <wiki_root>
"""
import json
import os
import select
import subprocess
import sys
import time

BIN = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "target", "debug", "agentwiki-mcp"))
WIKI = sys.argv[1]
env = dict(os.environ)
env["HOME"] = os.environ.get("MCP_CLIENT_HOME", "/tmp/mcp-probe-home")
# 预写配置：wiki_root 指向夹具（服务器从 ~/.agentwiki/config.json 读取）
cfg_dir = os.path.join(env["HOME"], ".agentwiki")
os.makedirs(cfg_dir, exist_ok=True)
with open(os.path.join(cfg_dir, "config.json"), "w") as f:
    json.dump({"wiki_root": WIKI, "embedding_model": None}, f)

proc = subprocess.Popen(
    [BIN], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env
)

def send(msg: dict) -> None:
    proc.stdin.write(json.dumps(msg).encode() + b"\n")
    proc.stdin.flush()

def recv(timeout: float = 30.0) -> dict:
    buf = b""
    deadline = time.time() + timeout
    while time.time() < deadline:
        r, _, _ = select.select([proc.stdout], [], [], 0.2)
        if r:
            chunk = os.read(proc.stdout.fileno(), 65536)
            if not chunk:
                raise TimeoutError(f"EOF; stderr={proc.stderr.read().decode(errors='replace')}")
            buf += chunk
            if b"\n" in buf:
                line, _, rest = buf.partition(b"\n")
                return json.loads(line.decode())
    raise TimeoutError(f"no response in {timeout}s; buf={buf[:200]!r}; stderr={proc.stderr.read().decode(errors='replace')[:300]}")

def main():
    # 1. initialize
    send({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "probe", "version": "0.1"}}})
    init = recv()
    print("== J1 initialize ==")
    print(json.dumps(init.get("result", {}), ensure_ascii=False)[:400])
    send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    # 2. tools/list
    send({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    tools = recv()
    print("\n== J1 tools/list ==")
    names = [t["name"] for t in tools["result"]["tools"]]
    print("tools:", names)

    # 3. get_wiki_context: 空查询（近期）
    send({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "get_wiki_context", "arguments": {"query": "", "limit": 5}}})
    r = recv()
    print("\n== J2 get_wiki_context(空查询) 原始返回 ==")
    print(json.dumps(r["result"], ensure_ascii=False)[:1500])

    # 4. get_wiki_context: 关键词
    send({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
        "name": "get_wiki_context", "arguments": {"query": "refresh token policy", "limit": 3}}})
    r = recv()
    print("\n== J2 get_wiki_context(关键词) 原始返回 ==")
    print(json.dumps(r["result"], ensure_ascii=False)[:900])
    print("\n== J2 get_wiki_context(关键词) 结构 ==")
    try:
        payload = json.loads(r["result"]["content"][0]["text"])
        print("顶层 keys:", list(payload.keys()))
        print("slices[0] keys:", list(payload["slices"][0].keys()) if payload.get("slices") else "无")
        print("related 样例:", payload.get("related"))
        print("strategy 存在?", "strategy" in payload, "| truncated 存在?", "truncated" in payload,
              "| match_sources 存在?", "match_sources" in (payload["slices"][0] if payload.get("slices") else {}))
    except Exception as e:
        print("解析失败:", e, r)

    # 5. get_wiki_rules
    send({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {
        "name": "get_wiki_rules", "arguments": {}}})
    r = recv()
    print("\n== J3 get_wiki_rules 结构 ==")
    try:
        payload = json.loads(r["result"]["content"][0]["text"])
        print("顶层 keys:", list(payload.keys()))
        print("内容:", json.dumps(payload, ensure_ascii=False)[:800])
    except Exception as e:
        print("解析失败:", e, r)

    # 6. validate_wiki
    send({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {
        "name": "validate_wiki", "arguments": {"path": "a.md"}}})
    r = recv()
    print("\n== J4 validate_wiki 结构 ==")
    try:
        payload = json.loads(r["result"]["content"][0]["text"])
        print("顶层 keys:", list(payload.keys()))
        print("issues 样例:", json.dumps(payload.get("issues", [])[:2], ensure_ascii=False))
    except Exception as e:
        print("解析失败:", e, r)

    # 7. 参数错误：path 与 full 互斥
    send({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {
        "name": "validate_wiki", "arguments": {"path": "a.md", "full": True}}})
    r = recv()
    print("\n== J4 validate_wiki(path+full) 互斥错误 ==")
    print("完整响应:", json.dumps(r, ensure_ascii=False)[:500])

    proc.terminate()

if __name__ == "__main__":
    main()