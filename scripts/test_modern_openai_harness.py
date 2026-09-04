#!/usr/bin/env python3
"""Exercise a built jcode CLI against a local Responses API with dummy credentials.

Usage: python3 scripts/test_modern_openai_harness.py target/debug/jcode
The test owns its daemon, socket, credentials, and files. It makes no model calls.
"""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def tool_events(calls):
    events = []
    for call in calls:
        events.append({"type": "response.output_item.added", "item": {**call, "arguments": ""}})
    # Interleave argument fragments, then send both completion forms. A tool
    # must still execute once with the arguments belonging to its own call ID.
    for half in (0, 1):
        for call in calls:
            arguments = call["arguments"]
            middle = len(arguments) // 2
            delta = arguments[:middle] if half == 0 else arguments[middle:]
            events.append({"type": "response.function_call_arguments.delta", "item_id": call["id"], "delta": delta})
    for call in calls:
        events.append({"type": "response.function_call_arguments.done", "item_id": call["id"], "arguments": call["arguments"]})
        events.append({"type": "response.output_item.done", "item": call})
    return events


def function_call(identifier, name, arguments):
    return {"type": "function_call", "id": f"fc_{identifier}", "call_id": f"call_{identifier}",
            "name": name, "arguments": json.dumps(arguments)}


def run_smoke(binary):
    with tempfile.TemporaryDirectory(prefix="jcode-astra-smoke-", dir="/tmp") as temporary:
        root = Path(temporary)
        work = root / "work"
        work.mkdir()
        (work / "alpha.txt").write_text("alpha=11\n")
        (work / "beta.txt").write_text("beta=31\n")
        expected_file = "alpha=11\nbeta=31\nsum=42\n"
        requests = []

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_GET(self):
                if self.path != "/v1/models":
                    self.send_error(404)
                    return
                body = json.dumps({"object": "list", "data": [{"id": "gpt-6-astra", "object": "model"}]}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_POST(self):
                if self.path != "/v1/responses":
                    self.send_error(404)
                    return
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(request)
                turn = len(requests)
                if turn > 3:
                    self.send_error(400, "Unexpected extra model turn")
                    return
                response_id = f"resp_{turn}"
                events = [{"type": "response.created", "response": {"id": response_id, "status": "in_progress"}}]
                if turn == 1:
                    events += tool_events([
                        function_call("alpha", "read", {"file_path": str(work / "alpha.txt")}),
                        function_call("beta", "read", {"file_path": str(work / "beta.txt")}),
                    ])
                elif turn == 2:
                    events += tool_events([function_call("result", "write", {
                        "file_path": str(work / "result.txt"), "content": expected_file,
                    })])
                else:
                    events.append({"type": "response.output_text.delta", "delta": "HARNESS_SMOKE_OK"})
                events.append({"type": "response.completed", "response": {
                    "id": response_id, "status": "completed",
                    "usage": {"input_tokens": 100, "output_tokens": 20},
                }})
                body = "".join(f"data: {json.dumps(event)}\n\n" for event in events).encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        api = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=api.serve_forever, daemon=True).start()
        # Keep normal OS variables, but isolate all jcode/provider overrides.
        env = {key: value for key, value in os.environ.items()
               if not key.startswith(("JCODE_", "OPENAI_", "ANTHROPIC_", "CLAUDE_"))}
        env.update({
            "JCODE_HOME": str(root / "home"),
            "JCODE_RUNTIME_DIR": str(root / "runtime"),
            "JCODE_OPENAI_API_BASE": f"http://127.0.0.1:{api.server_port}/v1",
            "JCODE_OPENAI_TRANSPORT": "https",
            "JCODE_OPENAI_REASONING_EFFORT": "high",
            "JCODE_NO_TELEMETRY": "1",
            "JCODE_MEMORY_ENABLED": "false",
            "JCODE_MEMORY_SIDECAR_ENABLED": "false",
            "JCODE_SWARM_ENABLED": "false",
            "JCODE_CHECK_UPDATES": "false",
            "OPENAI_API_KEY": "offline-smoke-key",
            "NO_PROXY": "127.0.0.1,localhost",
            "no_proxy": "127.0.0.1,localhost",
        })
        socket = root / "server.sock"
        command = [str(binary), "--no-update", "--no-selfdev", "--provider", "openai-api",
                   "--model", "gpt-6-astra", "--socket", str(socket), "--tools", "read,write", "-C", str(work)]
        try:
            with (root / "daemon.log").open("w+") as log:
                daemon = subprocess.Popen(command + ["serve", "--temporary-server", "--owner-pid", str(os.getpid())],
                                          env=env, cwd=work, stdout=log, stderr=log)
                try:
                    deadline = time.monotonic() + 30
                    while not socket.exists():
                        if daemon.poll() is not None or time.monotonic() >= deadline:
                            log.seek(0)
                            raise AssertionError(f"Isolated daemon did not start:\n{log.read()[-8000:]}")
                        time.sleep(0.05)
                    result = subprocess.run(command + ["run", "--ndjson", "Read alpha.txt and beta.txt, then write their values and sum to result.txt."],
                                            env=env, cwd=work, capture_output=True, text=True, timeout=60)
                    if result.returncode:
                        log.seek(0)
                        raise AssertionError(f"CLI exited {result.returncode}:\n{result.stderr[-4000:]}\n{log.read()[-8000:]}")
                    assert "HARNESS_SMOKE_OK" in result.stdout, result.stdout[-4000:]
                    events = [json.loads(line) for line in result.stdout.splitlines() if line.strip()]
                    done = [event for event in events if event.get("type") == "tool_done"]
                    assert Counter(event["id"] for event in done) == Counter({
                        "call_alpha": 1, "call_beta": 1, "call_result": 1,
                    }), done
                    assert all(not event.get("error") for event in done), done
                    assert (work / "result.txt").read_text() == expected_file
                finally:
                    daemon.terminate()
                    try:
                        daemon.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        daemon.kill()
                        daemon.wait(timeout=5)
        finally:
            api.shutdown()
            api.server_close()

        assert len(requests) == 3, f"Expected three model turns, got {len(requests)}"
        for request in requests:
            assert request["model"] == "gpt-6-astra"
            assert request["parallel_tool_calls"] is True
            assert request["prompt_cache_options"] == {"ttl": "30m"}
            assert "prompt_cache_retention" not in request
            assert request["reasoning"]["effort"] == "high"
            assert request["include"] == ["reasoning.encrypted_content"]
        outputs = [item for item in requests[1]["input"] if item.get("type") == "function_call_output"]
        assert len(outputs) == 2, outputs
        by_id = {item["call_id"]: item["output"] for item in outputs}
        assert set(by_id) == {"call_alpha", "call_beta"}, by_id
        assert "alpha=11" in by_id["call_alpha"], by_id
        assert "beta=31" in by_id["call_beta"], by_id
        final_outputs = [item for item in requests[2]["input"] if item.get("type") == "function_call_output"]
        assert [item["call_id"] for item in final_outputs].count("call_result") == 1, final_outputs
        print("PASS: Astra request settings, two interleaved reads, correlated results, one write, and final CLI response (3 model turns; local mock API).")


if __name__ == "__main__":
    binary = Path(sys.argv[1] if len(sys.argv) > 1 else "target/debug/jcode").resolve()
    if not binary.is_file():
        raise SystemExit(f"Build jcode first; no binary at {binary}")
    run_smoke(binary)
