#!/usr/bin/env python3
"""Capture real provider wire fixtures via a LiteLLM-compatible proxy.

Reads the virtual key ONLY from the environment (TARE_PROXY_KEY) and the proxy base
from ANTHROPIC_BASE_URL. Saves request/response BODIES only (no headers, no secret) into
the target directory (default: fixtures/). Covers both providers (Anthropic + OpenAI).

Usage:
    TARE_PROXY_KEY=sk-... ANTHROPIC_BASE_URL=https://.../llm-proxy \
        python3 scripts/capture_fixtures.py [target_dir]

This is the only step in the build that touches the network, and only loopback/​proxy.
"""
import json
import os
import sys
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

KEY = os.environ.get("TARE_PROXY_KEY") or ""
BASE = (os.environ.get("ANTHROPIC_BASE_URL") or "").rstrip("/")
TARGET = sys.argv[1] if len(sys.argv) > 1 else "fixtures"
A_MODEL = os.environ.get("TARE_ANTHROPIC_MODEL", "claude-opus-4-8")
O_MODEL = os.environ.get("TARE_OPENAI_MODEL", "gpt-5-mini")

if not KEY or not BASE:
    sys.exit("set TARE_PROXY_KEY and ANTHROPIC_BASE_URL in the environment")

BIG_SYS = (
    "You are an expert assistant operating under a fixed standing manual. "
    "Follow these procedures precisely and consistently across the whole session. "
) * 80  # ~comfortably over the 1024-token min cacheable prefix


def request(path, headers, body, stream=False, timeout=180):
    """POST one fixture request and fail before touching its response file.

    The standard-library client keeps credentials out of a child process's argv.
    Reading a streaming response to EOF is intentional: fixtures preserve the exact
    SSE body but do not need to process it incrementally while recording.
    """
    del stream
    req = Request(
        f"{BASE}{path}",
        data=json.dumps(body).encode("utf-8"),
        headers={**headers, "content-type": "application/json"},
        method="POST",
    )
    try:
        with urlopen(req, timeout=timeout) as response:
            return response.read().decode("utf-8")
    except HTTPError as error:
        raise RuntimeError(
            f"fixture request {path} failed with HTTP {error.code}"
        ) from error
    except URLError as error:
        raise RuntimeError(f"fixture request {path} failed: {error.reason}") from error


def anthropic(path, body, stream=False, timeout=180):
    return request(
        path,
        {"x-api-key": KEY, "anthropic-version": "2023-06-01"},
        body,
        stream,
        timeout,
    )


def openai(path, body, stream=False, timeout=180):
    return request(path, {"Authorization": f"Bearer {KEY}"}, body, stream, timeout)


def write(rel, content):
    full = os.path.join(TARGET, rel)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with open(full, "w", encoding="utf-8") as f:
        f.write(content)


def save_req(rel, body):
    write(rel, json.dumps(body, indent=2) + "\n")


def summarize(label, text):
    try:
        obj = json.loads(text)
        u = obj.get("usage") or obj.get("error") or {}
        print(f"  {label}: usage={json.dumps(u)[:200]}")
    except Exception:
        # SSE: pull the message_start / final usage-bearing lines
        line = next(
            (l for l in reversed(text.splitlines()) if '"usage"' in l), text[:120]
        )
        print(f"  {label}: {line[:200]}")


def main():
    print(f"capturing into {TARGET}/ (anthropic={A_MODEL}, openai={O_MODEL})")

    # (a) Anthropic non-stream
    b = {"model": A_MODEL, "max_tokens": 64, "system": "You summarize meeting notes concisely.",
         "messages": [{"role": "user", "content": "Summarize: we ship v0.1 Friday and defer dashboards."}]}
    save_req("anthropic_nonstream/request.json", b)
    r = anthropic("/v1/messages", b)
    write("anthropic_nonstream/response.json", r)
    summarize("a anthropic_nonstream", r)

    # (b) Anthropic stream with a cached 5m system prefix
    b = {"model": A_MODEL, "max_tokens": 64, "stream": True,
         "system": [{"type": "text", "text": BIG_SYS, "cache_control": {"type": "ephemeral"}}],
         "messages": [{"role": "user", "content": "Continue from where we left off."}]}
    save_req("anthropic_stream/request.json", b)
    r = anthropic("/v1/messages", b, stream=True)
    write("anthropic_stream/response.sse", r)
    summarize("b anthropic_stream", r)

    # (f) Anthropic two-turn cache with a 1h-TTL prefix: write then read
    sys1h = [{"type": "text", "text": "STABLE-PREFIX. " + BIG_SYS,
              "cache_control": {"type": "ephemeral", "ttl": "1h"}}]
    t1 = {"model": A_MODEL, "max_tokens": 64, "stream": True, "system": sys1h,
          "messages": [{"role": "user", "content": "Start the task: scaffold the module."}]}
    save_req("anthropic_cache_two_turn/turn1.request.json", t1)
    r = anthropic("/v1/messages", t1, stream=True)
    write("anthropic_cache_two_turn/turn1.response.sse", r)
    summarize("f turn1 (1h write)", r)
    t2 = {"model": A_MODEL, "max_tokens": 64, "stream": True, "system": sys1h,
          "messages": [{"role": "user", "content": "Start the task: scaffold the module."},
                       {"role": "assistant", "content": "Scaffolding the module now."},
                       {"role": "user", "content": "Now add the tests."}]}
    save_req("anthropic_cache_two_turn/turn2.request.json", t2)
    r = anthropic("/v1/messages", t2, stream=True)
    write("anthropic_cache_two_turn/turn2.response.sse", r)
    summarize("f turn2 (read)", r)

    # (thinking) Anthropic reasoning -> output_tokens_details.thinking_tokens
    b = {"model": A_MODEL, "max_tokens": 2048, "thinking": {"type": "adaptive"},
         "messages": [{"role": "user", "content": "A bat and ball cost $1.10; the bat costs $1 more than the ball. Reason step by step, then give the ball's price."}]}
    save_req("anthropic_thinking/request.json", b)
    r = anthropic("/v1/messages", b, timeout=240)
    write("anthropic_thinking/response.json", r)
    summarize("thinking anthropic", r)

    # (g) Bloated system prompt: 3 steps reusing a large UNCACHED system
    big_manual = "ENTERPRISE SUPPORT MANUAL (re-sent in full every step; never cached). " + BIG_SYS
    qs = ["How do I reset my password?", "How do I update my billing address?", "Why is my report export slow?"]
    for i, q in enumerate(qs, 1):
        b = {"model": A_MODEL, "max_tokens": 64, "system": big_manual,
             "messages": [{"role": "user", "content": q}]}
        save_req(f"bloated_system_prompt/step{i}.request.json", b)
        r = anthropic("/v1/messages", b)
        write(f"bloated_system_prompt/step{i}.response.json", r)
        summarize(f"g step{i}", r)

    # (h) Verbose tool output: a huge tool_result dominating the prompt
    logs = "\n".join(
        f"2026-06-25T10:00:{i:02d}Z {'ERROR' if i in (7, 25) else 'INFO'} payments req={i} "
        f"status={'500' if i in (7, 25) else '200'} route=/charge" for i in range(60)
    )
    b = {"model": A_MODEL, "max_tokens": 64, "system": "You analyze logs and answer briefly.",
         "tools": [{"name": "fetch_logs", "description": "Fetch recent logs.",
                    "input_schema": {"type": "object", "properties": {"service": {"type": "string"}}, "required": ["service"]}}],
         "messages": [
             {"role": "user", "content": "Did payments error in the last hour?"},
             {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_h_1", "name": "fetch_logs", "input": {"service": "payments"}}]},
             {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_h_1", "content": logs}]}]}
    save_req("verbose_tool_output/request.json", b)
    r = anthropic("/v1/messages", b)
    write("verbose_tool_output/response.json", r)
    summarize("h verbose_tool_output", r)

    # (c) OpenAI non-stream
    b = {"model": O_MODEL, "messages": [{"role": "system", "content": "You are concise."},
                                        {"role": "user", "content": "Say hello."}]}
    save_req("openai_nonstream/request.json", b)
    r = openai("/v1/chat/completions", b)
    write("openai_nonstream/response.json", r)
    summarize("c openai_nonstream", r)

    # (d) OpenAI stream WITH include_usage (reasoning-eliciting prompt -> reasoning_tokens)
    b = {"model": O_MODEL, "stream": True, "stream_options": {"include_usage": True},
         "messages": [{"role": "user", "content": "Plan a 3-step rollout; think it through, then list the 3 steps."}]}
    save_req("openai_stream_usage/request.json", b)
    r = openai("/v1/chat/completions", b, stream=True, timeout=240)
    write("openai_stream_usage/response.sse", r)
    summarize("d openai_stream_usage", r)

    # (d') OpenAI stream request that OMITS include_usage (proves proxy injection);
    #      response is captured WITH include_usage so the fake can replay a usage-bearing stream.
    save_req("openai_stream_no_usage/request.json",
             {"model": O_MODEL, "stream": True, "messages": [{"role": "user", "content": "Give me a one-line status."}]})
    b = {"model": O_MODEL, "stream": True, "stream_options": {"include_usage": True},
         "messages": [{"role": "user", "content": "Give me a one-line status."}]}
    r = openai("/v1/chat/completions", b, stream=True)
    write("openai_stream_no_usage/response.sse", r)
    summarize("d' openai_stream_no_usage", r)

    # (e) Retry loop 3x (OpenAI), identical request issued three times
    rb = {"model": O_MODEL, "messages": [{"role": "user", "content": "Return STRICT JSON: {\"status\":\"ok\"}"}]}
    for i in range(1, 4):
        save_req(f"retry_loop_3x/attempt{i}.request.json", rb)
        r = openai("/v1/chat/completions", rb)
        write(f"retry_loop_3x/attempt{i}.response.json", r)
        summarize(f"e attempt{i}", r)

    print("done.")


if __name__ == "__main__":
    main()
