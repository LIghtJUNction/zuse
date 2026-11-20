#!/usr/bin/env python3
"""Real MCP wire checks; all servers, state and optional Xvfb display are isolated."""
import argparse
import base64
import copy
import http.client
import json
import os
import pathlib
import queue
import re
import shutil
import signal
import socket
import stat
import struct
import subprocess
import tempfile
import threading
import time
import unittest

from private_xvfb import private_xvfb_command, verify_private_xvfb

ROOT = pathlib.Path(__file__).resolve().parents[1]
VERSION = "2026-07-28"
BINARY = None
META = {
    "io.modelcontextprotocol/protocolVersion": VERSION,
    "io.modelcontextprotocol/clientCapabilities": {},
}

def modern(method, params=None, ident=1, capabilities=None):
    values = dict(params or {})
    values["_meta"] = copy.deepcopy(META)
    if capabilities is not None:
        values["_meta"]["io.modelcontextprotocol/clientCapabilities"] = capabilities
    return {"jsonrpc": "2.0", "id": ident, "method": method, "params": values}

def tool(name, arguments=None, ident=1, capabilities=None, **extra):
    params = {"name": name}
    if arguments is not None:
        params["arguments"] = arguments
    params.update(extra)
    return modern("tools/call", params, ident, capabilities)

def check_tool(message):
    assert message.get("jsonrpc") == "2.0", message
    result = message["result"]
    if result.get("resultType") == "input_required":
        return result
    assert isinstance(result.get("content"), list), message
    textual = next(item["text"] for item in result["content"] if item["type"] == "text")
    decoded = json.loads(textual)
    assert result.get("structuredContent") == decoded, message
    return result

class Child:
    def __init__(self, command, env):
        self.process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, env=env, start_new_session=os.name == "posix")
        self.stdout = queue.Queue()
        self.stderr = queue.Queue()
        self.threads = []
        for pipe, messages in ((self.process.stdout, self.stdout), (self.process.stderr, self.stderr)):
            thread = threading.Thread(target=self.collect, args=(pipe, messages), daemon=True)
            thread.start()
            self.threads.append(thread)

    @staticmethod
    def collect(pipe, messages):
        try:
            for line in iter(pipe.readline, b""):
                messages.put(line)
        finally:
            messages.put(None)

    def line(self, stderr=False, timeout=8):
        messages = self.stderr if stderr else self.stdout
        try:
            line = messages.get(timeout=timeout)
        except queue.Empty as error:
            raise AssertionError("process produced no complete response before deadline") from error
        if line is None:
            raise AssertionError("process closed its stream: exit=" + str(self.process.poll()))
        return line

    def send(self, value):
        raw = value if isinstance(value, bytes) else json.dumps(value, ensure_ascii=False).encode()
        self.process.stdin.write(raw + b"\n")
        self.process.stdin.flush()

    def call(self, request):
        self.send(request)
        response = json.loads(self.line())
        assert response.get("id") == request.get("id"), (request, response)
        return response

    def close(self):
        if self.process.poll() is None:
            if os.name == "posix":
                os.killpg(self.process.pid, signal.SIGTERM)
            else:
                self.process.terminate()
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                if os.name == "posix":
                    os.killpg(self.process.pid, signal.SIGKILL)
                else:
                    self.process.kill()
                self.process.wait(timeout=3)
        for pipe in (self.process.stdin, self.process.stdout, self.process.stderr):
            pipe.close()
        for thread in self.threads:
            thread.join(timeout=1)

class HttpServer(Child):
    def __init__(self, env, *flags):
        super().__init__([BINARY, "mcp", "--host", "127.0.0.1", "--port", "0", *flags], env)
        line = self.line(stderr=True).decode()
        match = re.search(r"http://127\.0\.0\.1:([0-9]+)/mcp", line)
        if not match:
            self.close()
            raise AssertionError("unexpected HTTP startup: " + line)
        self.port = int(match.group(1))

    def request(self, value, headers=None, path="/mcp", method="POST"):
        body = value if isinstance(value, bytes) else json.dumps(value).encode()
        fields = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream",
                  "MCP-Protocol-Version": VERSION}
        if isinstance(value, dict):
            fields["Mcp-Method"] = value.get("method", "")
            if "name" in value.get("params", {}):
                fields["Mcp-Name"] = value["params"]["name"]
        fields.update(headers or {})
        fields = {key: value for key, value in fields.items() if value is not None}
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        try:
            connection.request(method, path, body=body, headers=fields)
            response = connection.getresponse()
            payload = response.read()
            return response.status, json.loads(payload) if payload else None
        finally:
            connection.close()

class Transports(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workspace = tempfile.TemporaryDirectory(prefix="zuse-transport-tests-")
        cls.display = None
        cls.xvfb = None
        if os.name == "posix" and shutil.which("Xvfb"):
            xvfb_command, display_number = private_xvfb_command("64x48x24", extra=("-ac",))
            cls.xvfb = Child(xvfb_command, os.environ.copy())
            try:
                cls.display = verify_private_xvfb(cls.xvfb.process, display_number, cls.xvfb.line())
            except BaseException:
                cls.xvfb.close()
                cls.workspace.cleanup()
                raise

    @classmethod
    def tearDownClass(cls):
        if cls.xvfb:
            cls.xvfb.close()
        cls.workspace.cleanup()

    def setUp(self):
        self.directory = pathlib.Path(tempfile.mkdtemp(dir=self.workspace.name))
        self.env = os.environ.copy()
        for name in ("ZUSE_TOKEN", "WAYLAND_DISPLAY", "HYPRLAND_INSTANCE_SIGNATURE"):
            self.env.pop(name, None)
        self.env["ZUSE_STATE_DIR"] = str(self.directory / "state")
        self.env["DBUS_SESSION_BUS_ADDRESS"] = "unix:path=/does-not-exist/zuse-test-bus"
        if self.display:
            self.env["DISPLAY"] = self.display
        else:
            self.env.pop("DISPLAY", None)
        self.children = []

    def tearDown(self):
        for child in reversed(self.children):
            child.close()

    def stdio(self, *flags):
        child = Child([BINARY, "mcp", "--stdio", *flags], self.env)
        self.children.append(child)
        return child

    def http(self, *flags):
        child = HttpServer(self.env, *flags)
        self.children.append(child)
        return child

    def test_legacy_handshake_and_tool_call(self):
        server = self.stdio()
        initialized = server.call({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                       "clientInfo": {"name": "zuse-tests", "version": "1"}}})
        self.assertEqual(initialized["result"]["serverInfo"]["name"], "zuse")
        server.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        listed = server.call({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        self.assertIn("zuse.execute", [item["name"] for item in listed["result"]["tools"]])
        reply = server.call({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                             "params": {"name": "zuse.check", "arguments": {"code": "mouse.position()"}}})
        self.assertFalse(check_tool(reply)["isError"])

    def test_modern_stdio_and_argument_contract(self):
        server = self.stdio()
        listed = server.call(modern("tools/list"))
        self.assertGreaterEqual(len(listed["result"]["tools"]), 8)
        self.assertFalse(check_tool(server.call(tool("zuse.execute", {"code": "wait(ms=0)"}, 2)))["isError"])
        bad = [
            ("zuse.check", {}),
            ("zuse.execute", {"code": 7}),
            ("zuse.execute", {"code": "wait(ms=0)", "dry_run": "true"}),
            ("zuse.capabilities", {"unexpected": True}),
            ("zuse.observe", {"mode": "invented"}),
            ("zuse.screenshot", {"window": 12}),
            ("zuse.resume", {}),
        ]
        for index, (name, arguments) in enumerate(bad, 3):
            response = server.call(tool(name, arguments, index))
            self.assertEqual(response["error"]["code"], -32602, response)
        observation = server.call(tool("zuse.observe", {"mode": "text"}, 30))
        check_tool(observation)  # Missing accessibility is a structured capability error.
        self.assertFalse(check_tool(server.call(tool("zuse.schema", {}, 31)))["isError"])

    def test_errors_preserve_stream_and_request_ids(self):
        server = self.stdio()
        server.send(b"{broken")
        self.assertEqual(json.loads(server.line())["error"]["code"], -32700)
        server.send(b"[]")
        self.assertEqual(json.loads(server.line())["error"]["code"], -32600)
        invalid = modern("tools/call", {"name": "zuse.execute", "arguments": []}, 9)
        self.assertEqual(server.call(invalid)["error"]["code"], -32602)
        wrong_state = tool("zuse.execute", {"code": "wait(ms=0)"}, 10, requestState=False)
        self.assertEqual(server.call(wrong_state)["error"]["code"], -32602)
        self.assertIn("tools", server.call(modern("tools/list", ident=11))["result"])

    def test_server_read_only_cannot_be_weakened(self):
        server = self.stdio("--read-only")
        response = server.call(tool("zuse.execute", {"code": "mouse.click()", "read_only": False}))
        self.assertTrue(check_tool(response)["isError"])
        self.assertFalse(check_tool(server.call(tool("zuse.check", {"code": "mouse.position()"}, 2)))["isError"])

    def test_dry_run_has_no_human_side_effect(self):
        server = self.stdio("--dry-run")
        response = server.call(tool("zuse.execute", {"code": 'human.request(kind="approval",message="test")'}))
        self.assertFalse(check_tool(response)["isError"])
        self.assertFalse(list(self.directory.glob("state/human/*.json")))

    def test_human_retry_integrity_and_single_use(self):
        server = self.stdio()
        capabilities = {"elicitation": {"form": {}}}
        code = 'human.request(kind="approval",message="transport test",timeout=30); wait(ms=0)'
        args = {"code": code}
        first = server.call(tool("zuse.execute", args, 1, capabilities))
        requested = first["result"]
        self.assertEqual(requested["resultType"], "input_required")
        self.assertEqual(requested["inputRequests"]["confirm"]["params"]["mode"], "form")
        state = requested["requestState"]
        self.assertTrue(state.startswith("zuse-mrtr-"))
        accept = {"confirm": {"action": "accept", "content": {"completed": True}}}
        for ident, fields, changed in [
            (2, {"requestState": state + "0", "inputResponses": accept}, args),
            (3, {"requestState": state, "inputResponses": accept}, {"code": code + "; wait(ms=0)"}),
            (4, {"requestState": state, "inputResponses": {"confirm": {"action": "accept", "content": {"completed": "yes"}}}}, args),
            (5, {"requestState": state, "inputResponses": {"confirm": {"action": "accept", "content": {"completed": True, "password": "never-store-this"}}}}, args),
            (1, {"requestState": state, "inputResponses": accept}, args),
        ]:
            response = server.call(tool("zuse.execute", changed, ident, capabilities, **fields))
            self.assertTrue(check_tool(response)["isError"], response)
        record = json.loads(next(self.directory.glob("state/human/*.json")).read_text())
        response = server.call(tool("zuse.execute", args, 6, capabilities, requestState=record["id"], inputResponses=accept))
        self.assertTrue(check_tool(response)["isError"])
        other = self.stdio()
        response = other.call(tool("zuse.execute", args, 7, capabilities, requestState=state, inputResponses=accept))
        self.assertTrue(check_tool(response)["isError"])
        response = server.call(tool("zuse.execute", args, 8, capabilities, requestState=state, inputResponses=accept))
        result = check_tool(response)
        self.assertFalse(result["isError"])
        self.assertEqual(result["structuredContent"]["status"], "completed")
        self.assertEqual(result["structuredContent"]["completed"], 1)
        replay = server.call(tool("zuse.execute", args, 9, capabilities, requestState=state, inputResponses=accept))
        self.assertTrue(check_tool(replay)["isError"])
        for path in self.directory.glob("state/human/*"):
            self.assertNotIn("never-store-this", path.read_text())

    def test_password_stays_local_and_url_only_client_gets_local_handoff(self):
        server = self.stdio()
        password = 'human.request(kind="password",message="Enter locally",window="123",timeout=30)'
        result = check_tool(server.call(tool("zuse.execute", {"code": password}, 1, {"elicitation": {"form": {}}})))
        self.assertEqual(result["structuredContent"]["status"], "waiting_for_human")
        self.assertNotEqual(result.get("resultType"), "input_required")
        approval = 'human.request(kind="approval",message="Local confirmation",timeout=30)'
        result = check_tool(server.call(tool("zuse.execute", {"code": approval}, 2, {"elicitation": {"url": {}}})))
        self.assertEqual(result["structuredContent"]["status"], "waiting_for_human")

    def test_http_metadata_origin_host_and_json_errors(self):
        server = self.http()
        status, response = server.request(modern("tools/list"))
        self.assertEqual(status, 200)
        self.assertIn("tools", response["result"])
        status, response = server.request(tool("zuse.check", {"code": "wait(ms=0)"}))
        self.assertEqual(status, 200)
        self.assertFalse(check_tool(response)["isError"])
        for headers, expected in [
            ({"Origin": "https://attacker.invalid"}, 403),
            ({"Host": "attacker.invalid"}, 401),
            ({"Mcp-Method": "wrong"}, 400),
            ({"Mcp-Method": None}, 400),
            ({"Content-Type": "text/plain"}, 415),
            ({"Accept": "text/plain"}, 406),
        ]:
            status, _ = server.request(modern("tools/list"), headers)
            self.assertEqual(status, expected, headers)
        status, response = server.request(b"{", {"Mcp-Method": "tools/list"})
        self.assertEqual(status, 400)
        self.assertEqual(response["error"]["code"], -32700)
        status, response = server.request(b"[]", {"Mcp-Method": "tools/list"})
        self.assertEqual(status, 400)
        self.assertEqual(response["error"]["code"], -32600)
        status, response = server.request(modern("not/a/method"))
        self.assertEqual(status, 404)
        self.assertEqual(response["error"]["code"], -32601)
        self.assertEqual(server.request(modern("tools/list"), path="/wrong")[0], 404)
        self.assertEqual(server.request(b"", method="GET")[0], 405)
        self.assertEqual(server.request(b"", {"Content-Length": str(2 * 1024 * 1024)})[0], 413)
        status, _ = server.request(modern("tools/list"))
        self.assertEqual(status, 200)

    def test_http_token_and_name_binding(self):
        self.env["ZUSE_TOKEN"] = "zuse-test-token-" + "x" * 32
        server = self.http()
        request = tool("zuse.check", {"code": "wait(ms=0)"})
        self.assertEqual(server.request(request)[0], 401)
        self.assertEqual(server.request(request, {"Authorization": "Bearer wrong"})[0], 401)
        auth = {"Authorization": "Bearer " + self.env["ZUSE_TOKEN"]}
        self.assertEqual(server.request(request, auth)[0], 200)
        self.assertEqual(server.request(request, dict(auth, **{"Mcp-Name": "other"}))[0], 400)

    @unittest.skipUnless(hasattr(socket, "AF_UNIX") and os.name == "posix", "Unix socket platform")
    def test_unix_socket_permissions_and_jsonl(self):
        path = self.directory / "mcp.sock"
        server = Child([BINARY, "mcp", "--socket", str(path)], self.env)
        self.children.append(server)
        self.assertIn(b"zuse MCP unix", server.line(stderr=True))
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        with socket.socket(socket.AF_UNIX) as client:
            client.settimeout(5)
            client.connect(str(path))
            client.sendall(json.dumps(modern("tools/list")).encode() + b"\n")
            with client.makefile("rb") as reader:
                response = json.loads(reader.readline())
            self.assertIn("tools", response["result"])

    def test_inline_screenshot_is_a_real_isolated_png(self):
        if not self.display:
            self.skipTest("Xvfb is unavailable")
        server = self.stdio()
        response = server.call(tool("zuse.screenshot", {"source": "desktop"}))
        result = check_tool(response)
        self.assertFalse(result["isError"], result)
        image = next(item for item in result["content"] if item["type"] == "image")
        self.assertEqual(image["mimeType"], "image/png")
        data = base64.b64decode(image["data"], validate=True)
        self.assertTrue(data.startswith(b"\x89PNG\r\n\x1a\n"))
        self.assertEqual(struct.unpack(">II", data[16:24]), (64, 48))

    def test_frame_limit_is_bounded(self):
        child = subprocess.run([BINARY, "mcp", "--stdio"], input=b"x" * (1024 * 1024 + 20) + b"\n",
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=self.env, timeout=5)
        self.assertNotEqual(child.returncode, 0)
        self.assertIn(b"exceeds 1 MiB", child.stderr)
        self.assertLess(len(child.stdout) + len(child.stderr), 8192)

def main():
    global BINARY
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=pathlib.Path)
    args, remaining = parser.parse_known_args()
    if args.binary:
        BINARY = str(args.binary.resolve())
    else:
        metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"], cwd=ROOT, text=True))
        BINARY = str(pathlib.Path(metadata["target_directory"]) / "debug" / ("zuse.exe" if os.name == "nt" else "zuse"))
    if not pathlib.Path(BINARY).is_file():
        parser.error("build the binary with cargo build --locked --bin zuse first")
    unittest.main(argv=[__file__, *remaining], verbosity=2)

if __name__ == "__main__":
    main()
