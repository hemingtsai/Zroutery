#!/usr/bin/env python3
"""Smoke test the whole stack: mock provider -> zroutery-headless -> client.

Starts a fake OpenAI compatible provider, writes a config that mirrors the
product brief (DeepSeek flash/pro + OpenAI, plus the three classes), boots the
headless proxy and exercises both dialects, streaming and non streaming.

Usage: python3 scripts/smoke_test.py [path-to-zroutery-headless]
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOKEN = "zr-smoke-token"
FAILURES: list[str] = []


class Provider(BaseHTTPRequestHandler):
    """Answers /chat/completions; the model name selects the behaviour."""

    protocol_version = "HTTP/1.1"
    seen: list[dict] = []

    def log_message(self, *_args):  # keep the output clean
        pass

    def _send(self, code: int, payload: bytes, ctype="application/json"):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        self._send(200, json.dumps({"data": [{"id": "mock-1"}, {"id": "mock-2"}]}).encode())

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        Provider.seen.append({"auth": self.headers.get("Authorization"), "body": body})
        model = body.get("model", "")

        if model.startswith("broken"):
            self._send(500, json.dumps({"error": {"message": "mock failure"}}).encode())
            return

        if not body.get("stream"):
            self._send(
                200,
                json.dumps(
                    {
                        "id": "chatcmpl-mock",
                        "object": "chat.completion",
                        "model": model,
                        "choices": [
                            {
                                "index": 0,
                                "message": {"role": "assistant", "content": f"reply from {model}"},
                                "finish_reason": "stop",
                            }
                        ],
                        "usage": {"prompt_tokens": 8, "completion_tokens": 4},
                    }
                ).encode(),
            )
            return

        chunks = [
            {"choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": None}]},
            {"choices": [{"index": 0, "delta": {"reasoning_content": "thinking"}, "finish_reason": None}]},
            {"choices": [{"index": 0, "delta": {"content": "streamed "}, "finish_reason": None}]},
            {"choices": [{"index": 0, "delta": {"content": "answer"}, "finish_reason": None}]},
            {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
            {"choices": [], "usage": {"prompt_tokens": 3, "completion_tokens": 9}},
        ]
        payload = "".join(
            "data: " + json.dumps({"id": "chatcmpl-mock", "model": model, **c}) + "\n\n" for c in chunks
        )
        payload += "data: [DONE]\n\n"
        self._send(200, payload.encode(), "text/event-stream")


def request(url: str, body: dict | None = None, token: str | None = TOKEN, stream=False):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method="POST" if body else "GET")
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("x-api-key", token)
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            raw = resp.read().decode()
            return resp.status, lower_headers(resp.headers), raw if stream else json.loads(raw or "{}")
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, lower_headers(e.headers), json.loads(raw or "{}")
        except json.JSONDecodeError:
            return e.code, lower_headers(e.headers), raw


def lower_headers(headers) -> dict:
    """HTTP header names are case insensitive; normalise for assertions."""
    return {k.lower(): v for k, v in headers.items()}


def check(name: str, condition: bool, detail: str = ""):
    if condition:
        print(f"  ok   {name}")
    else:
        FAILURES.append(name)
        print(f"  FAIL {name} {detail}")


def write_config(path: str, upstream: str):
    config = {
        "server": {
            "host": "127.0.0.1",
            "port": 8791,
            "require_auth": True,
            "auth_token": TOKEN,
            "autostart": True,
            "allow_cors": False,
            "log_limit": 100,
        },
        "routing": {
            "strategy": "priority",
            "failover": True,
            "max_attempts": 3,
            "break_after_failures": 2,
            "cooldown_secs": 30,
            "unknown_model_fallback": None,
            "client_aliases": {},
            "match_claude_names": True,
        },
        "providers": [
            {
                "id": "deepseek",
                "name": "DeepSeek",
                "kind": "openai_compatible",
                "base_url": upstream,
                "key_ref": "provider:deepseek",
                "enabled": True,
                "timeout_secs": 30,
            },
            {
                "id": "openai",
                "name": "OpenAI",
                "kind": "openai_compatible",
                "base_url": upstream,
                "key_ref": "provider:openai",
                "enabled": True,
                "timeout_secs": 30,
            },
        ],
        "models": [
            # Ids are derived as <provider>-<model>, so no entry carries an "id"
            # except the legacy one at the end.
            {
                "provider_id": "deepseek",
                "upstream_model": "deepseek-v4-flash",
                "class": "haiku",
            },
            {
                "provider_id": "deepseek",
                "upstream_model": "deepseek-v4-pro",
                "class": "sonnet",
            },
            # The very same model name, offered by a second provider.
            {
                "provider_id": "openai",
                "upstream_model": "deepseek-v4-pro",
                "class": "sonnet",
                "priority": 50,
            },
            {
                "provider_id": "openai",
                "upstream_model": "gpt-5.3-sol",
                "class": "opus",
                "priority": 0,
            },
            {
                "provider_id": "openai",
                "upstream_model": "broken-model",
                "class": "opus",
                "priority": -10,
            },
            {"provider_id": "openai", "upstream_model": "mystery"},
            # Written by 0.1.x: the free-form id must survive as an alias.
            {
                "id": "legacy-name",
                "provider_id": "openai",
                "upstream_model": "gpt-legacy",
                "class": "haiku",
                "priority": 90,
            },
        ],
    }
    with open(os.path.join(path, "config.json"), "w") as fh:
        json.dump(config, fh, indent=2)


def main() -> int:
    binary = sys.argv[1] if len(sys.argv) > 1 else "target/debug/zroutery-headless"
    if not os.path.exists(binary):
        print(f"binary not found: {binary}")
        return 2

    server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    upstream = f"http://127.0.0.1:{server.server_port}"
    threading.Thread(target=server.serve_forever, daemon=True).start()

    config_dir = tempfile.mkdtemp(prefix="zroutery-smoke-")
    write_config(config_dir, upstream)

    env = dict(os.environ)
    env["ZROUTERY_CONFIG_DIR"] = config_dir
    env["ZROUTERY_KEY_PROVIDER_DEEPSEEK"] = "sk-mock-deepseek"
    env["ZROUTERY_KEY_PROVIDER_OPENAI"] = "sk-mock-openai"
    env["ZROUTERY_LOG"] = "warn"
    proxy = subprocess.Popen([binary], env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)

    base = "http://127.0.0.1:8791"
    try:
        for _ in range(100):
            try:
                status, _, _ = request(f"{base}/health", token=None)
                if status == 200:
                    break
            except Exception:
                time.sleep(0.1)
        else:
            print("proxy never became healthy")
            print(proxy.stdout.read() if proxy.stdout else "")
            return 1

        print("auth")
        status, _, _ = request(
            f"{base}/v1/messages",
            {"model": "sonnet-class", "max_tokens": 8, "messages": [{"role": "user", "content": "hi"}]},
            token=None,
        )
        check("unauthenticated requests are rejected", status == 401, f"got {status}")

        print("model listing")
        _, _, listing = request(f"{base}/v1/models")
        ids = [m["id"] for m in listing["data"]]
        check(
            "classes and concrete models are listed",
            {
                "opus-class",
                "sonnet-class",
                "haiku-class",
                "deepseek-deepseek-v4-pro",
                "openai-deepseek-v4-pro",
                "openai-mystery",
            }
            <= set(ids),
            str(ids),
        )
        unclassified = next(m for m in listing["data"] if m["id"] == "openai-mystery")
        check("unclassified model has no class", unclassified["zroutery"]["class"] is None)

        print("anthropic dialect, non streaming")
        status, headers, body = request(
            f"{base}/v1/messages",
            {"model": "sonnet-class", "max_tokens": 16, "system": "be brief",
             "messages": [{"role": "user", "content": "hello"}]},
        )
        check("200 from sonnet-class", status == 200, str(body))
        check(
            "routed to deepseek-deepseek-v4-pro",
            headers.get("x-zroutery-model") == "deepseek-deepseek-v4-pro",
            str(headers),
        )
        check("anthropic response shape", body.get("type") == "message" and body["content"][0]["type"] == "text")
        check("upstream text is returned", "deepseek-v4-pro" in body["content"][0]["text"], str(body))
        check("usage is reported", body["usage"]["input_tokens"] == 8)
        sent = Provider.seen[-1]
        check("upstream got a bearer token", sent["auth"] == "Bearer sk-mock-deepseek", str(sent["auth"]))
        check("system prompt became a system message", sent["body"]["messages"][0]["role"] == "system")

        print("openai dialect, non streaming")
        status, headers, body = request(
            f"{base}/v1/chat/completions",
            {"model": "haiku-class", "messages": [{"role": "user", "content": "hello"}]},
        )
        check("200 from haiku-class", status == 200, str(body))
        check(
            "routed to deepseek-deepseek-v4-flash",
            headers.get("x-zroutery-model") == "deepseek-deepseek-v4-flash",
        )
        check("openai response shape", body.get("object") == "chat.completion")
        check("finish reason mapped", body["choices"][0]["finish_reason"] == "stop")

        print("failover inside a class")
        status, headers, body = request(
            f"{base}/v1/messages",
            {"model": "opus-class", "max_tokens": 16, "messages": [{"role": "user", "content": "hi"}]},
        )
        check(
            "failed over to the healthy opus model",
            headers.get("x-zroutery-model") == "openai-gpt-5.3-sol",
            str(headers),
        )
        check("client still sees 200", status == 200)

        print("claude style name")
        _, headers, _ = request(
            f"{base}/v1/messages",
            {"model": "claude-3-5-haiku-20241022", "max_tokens": 8,
             "messages": [{"role": "user", "content": "hi"}]},
        )
        check(
            "claude haiku name maps to haiku-class",
            headers.get("x-zroutery-model") == "deepseek-deepseek-v4-flash",
        )

        print("the same model from two providers")
        for model_id, expected_key in [
            ("deepseek-deepseek-v4-pro", "Bearer sk-mock-deepseek"),
            ("openai-deepseek-v4-pro", "Bearer sk-mock-openai"),
        ]:
            _, headers, _ = request(
                f"{base}/v1/messages",
                {"model": model_id, "max_tokens": 16,
                 "messages": [{"role": "user", "content": "hi"}]},
            )
            sent = Provider.seen[-1]
            check(f"{model_id} reaches its own provider", sent["auth"] == expected_key, str(sent["auth"]))
            check(f"{model_id} sends the bare model name", sent["body"]["model"] == "deepseek-v4-pro")
            check(f"{model_id} is reported back", headers.get("x-zroutery-model") == model_id)

        print("ids from 0.1.x")
        status, headers, _ = request(
            f"{base}/v1/messages",
            {"model": "legacy-name", "max_tokens": 16,
             "messages": [{"role": "user", "content": "hi"}]},
        )
        check("the old id still resolves", status == 200)
        check(
            "and reports the new id",
            headers.get("x-zroutery-model") == "openai-gpt-legacy",
            str(headers),
        )
        with open(os.path.join(config_dir, "config.json")) as fh:
            migrated = json.load(fh)
        legacy = next(m for m in migrated["models"] if m["upstream_model"] == "gpt-legacy")
        check("the old id was written back as an alias", legacy.get("aliases") == ["legacy-name"], str(legacy))
        check("and the free-form id is gone", "id" not in legacy, str(legacy))

        print("streaming, anthropic dialect over an openai provider")
        _, headers, wire = request(
            f"{base}/v1/messages",
            {"model": "sonnet-class", "max_tokens": 16, "stream": True,
             "messages": [{"role": "user", "content": "hi"}]},
            stream=True,
        )
        check("event stream content type", headers.get("content-type") == "text/event-stream")
        check("message_start first", wire.startswith("event: message_start"))
        check("message_stop last", wire.strip().endswith('{"type":"message_stop"}'))
        check("reasoning became a thinking block", '"type":"thinking"' in wire)
        check("text deltas present", '"text":"streamed "' in wire and '"text":"answer"' in wire)
        check("two blocks opened and closed",
              wire.count("event: content_block_start") == 2 and wire.count("event: content_block_stop") == 2)
        check("usage forwarded", '"output_tokens":9' in wire)

        print("streaming, openai dialect")
        _, _, wire = request(
            f"{base}/v1/chat/completions",
            {"model": "sonnet-class", "stream": True, "stream_options": {"include_usage": True},
             "messages": [{"role": "user", "content": "hi"}]},
            stream=True,
        )
        check("chunk objects", '"object":"chat.completion.chunk"' in wire)
        check("reasoning_content preserved", '"reasoning_content":"thinking"' in wire)
        check("terminated with DONE", wire.strip().endswith("data: [DONE]"))
        check("usage trailer present", '"completion_tokens":9' in wire)

        print("errors")
        status, _, body = request(
            f"{base}/v1/chat/completions",
            {"model": "nope-nope", "messages": [{"role": "user", "content": "hi"}]},
        )
        check("unknown model is a 404", status == 404, str(body))
        check("openai error shape", "error" in body and body["error"]["code"] == "not_found_error")

        status, _, body = request(
            f"{base}/v1/messages/count_tokens",
            {"model": "sonnet-class", "messages": [{"role": "user", "content": "some text to count"}]},
        )
        check("count_tokens answers", status == 200 and body["input_tokens"] > 0, str(body))

        print("config on disk carries no secrets")
        with open(os.path.join(config_dir, "config.json")) as fh:
            text = fh.read()
        check("no api keys persisted", "sk-mock" not in text)

    finally:
        proxy.terminate()
        try:
            proxy.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proxy.kill()
        server.shutdown()
        shutil.rmtree(config_dir, ignore_errors=True)

    print()
    if FAILURES:
        print(f"{len(FAILURES)} check(s) failed: {', '.join(FAILURES)}")
        return 1
    print("all smoke checks passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
