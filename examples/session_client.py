#!/usr/bin/env python3
"""Submit stdin Zuse source and wait on one public session-protocol connection.

Use the exact socket path returned by an observed `zuse session start/status`:
    python3 examples/session_client.py --socket /observed/session.sock < job.zuse
    python3 examples/session_client.py --socket /observed/session.sock --observe

Input must end with EOF. The client sends source once, then polls `result` on
the same connection. Relative imports use --base (the client's cwd by default).
--timeout-ms bounds stdin, connection and result waiting together; it does not
cancel a timed-out job. --max-wait-ms is the separate Zuse execution budget.

Stdout is the final server JSON, or a client_status diagnostic with the known
job/revision on timeout, disconnect or Ctrl+C. No source is retried. Ctrl+C
attempts one revision-guarded cancel on a fresh connection to the same socket
instance; cleanup still requires a terminal result. The session stays open.
Exit codes: 0 completed, 1 server/job/client error, 124 timeout, 130 Ctrl+C.
Requires Python 3.10+ on a platform with Unix sockets and selectable stdin.
"""

import argparse
import json
import os
import select
import socket
import stat
import sys
import time
import uuid

MAX_SOURCE = 256 * 1024
MAX_FRAME = 2 * 1024 * 1024


def integer(low, high):
    def parse(value):
        number = int(value)
        if not low <= number <= high:
            raise argparse.ArgumentTypeError(f"must be between {low} and {high}")
        return number
    return parse


def request_id(value):
    if not 1 <= len(value.encode("utf-8")) <= 128 or any(
        ord(c) < 32 or 127 <= ord(c) <= 159 for c in value
    ):
        raise argparse.ArgumentTypeError("request ID needs 1..128 UTF-8 bytes without controls")
    return value


def remaining(deadline):
    seconds = deadline - time.monotonic()
    if seconds <= 0:
        raise TimeoutError("client deadline expired")
    return seconds


def source_from_stdin(deadline):
    data = bytearray()
    while True:
        if not select.select([sys.stdin.fileno()], [], [], remaining(deadline))[0]:
            raise TimeoutError("stdin deadline expired")
        chunk = os.read(sys.stdin.fileno(), min(65536, MAX_SOURCE + 1 - len(data)))
        if not chunk:
            return data.decode("utf-8")
        data.extend(chunk)
        if len(data) > MAX_SOURCE:
            raise ValueError("source exceeds 256 KiB")


def socket_identity(path):
    info = os.lstat(path)
    if not stat.S_ISSOCK(info.st_mode):
        raise ValueError("--socket must name the observed Unix socket itself")
    return info.st_dev, info.st_ino


class Connection:
    def __init__(self, path, deadline, identity=None):
        self.deadline = deadline
        self.identity = socket_identity(path)
        if identity is not None and self.identity != identity:
            raise ValueError("socket instance changed; no cancellation sent")
        self.buffer = bytearray()
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            self.sock.settimeout(remaining(deadline))
            self.sock.connect(path)
            if socket_identity(path) != self.identity:
                raise ValueError("socket instance changed during connection")
        except BaseException:
            self.sock.close()
            raise

    def close(self):
        self.sock.close()

    def exchange(self, request):
        wire = (json.dumps(request, ensure_ascii=False, separators=(",", ":")) + "\n").encode()
        if len(wire) > MAX_FRAME:
            raise ValueError("request frame exceeds 2 MiB")
        self.sock.settimeout(remaining(self.deadline))
        self.sock.sendall(wire)
        while True:
            end = self.buffer.find(b"\n")
            if end >= 0:
                if end + 1 > MAX_FRAME:
                    raise ValueError("response frame exceeds 2 MiB")
                reply = json.loads(self.buffer[:end])
                del self.buffer[:end + 1]
                if not isinstance(reply, dict) or type(reply.get("version")) is not int:
                    raise ValueError("invalid protocol response")
                if reply["version"] != 1 or reply.get("request_id") != request["request_id"]:
                    raise ValueError("response version or request ID mismatch")
                if type(reply.get("ok")) is not bool:
                    raise ValueError("response is missing its boolean ok field")
                if type(reply.get("state_revision")) is not int or not 1 <= reply["state_revision"] < 2**64:
                    raise ValueError("response has no valid state revision")
                return reply
            if len(self.buffer) >= MAX_FRAME:
                raise ValueError("response frame exceeds 2 MiB")
            self.sock.settimeout(remaining(self.deadline))
            chunk = self.sock.recv(min(65536, MAX_FRAME - len(self.buffer)))
            if not chunk:
                raise EOFError("session disconnected before a complete response")
            self.buffer.extend(chunk)


def message(op, **fields):
    return {"version": 1, "request_id": uuid.uuid4().hex, "op": op, **fields}


def emit(value):
    print(json.dumps(value, ensure_ascii=False, separators=(",", ":")), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--socket", required=True, help="explicit, observed Unix socket path")
    parser.add_argument("--observe", action="store_true", help="submit a forced read-only job")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--base", default=os.getcwd(), help="absolute import directory (default: cwd)")
    parser.add_argument("--request-id", type=request_id, default=uuid.uuid4().hex)
    parser.add_argument("--expect-revision", type=integer(0, 2**64 - 1))
    parser.add_argument("--max-steps", type=integer(1, 65536))
    parser.add_argument("--max-wait-ms", type=integer(0, 86400000))
    parser.add_argument("--timeout-ms", type=integer(1, 3600000), default=30000)
    parser.add_argument("--poll-ms", type=integer(10, 1000), default=25)
    args = parser.parse_args()
    if not os.path.isabs(args.socket) or not os.path.isabs(args.base):
        parser.error("--socket and --base must be absolute paths")

    deadline = time.monotonic() + args.timeout_ms / 1000
    known = {"request_id": args.request_id, "job_id": None, "state": None,
             "state_revision": None, "last_status": None, "submission_attempted": False}
    connection = None

    def remember(reply):
        for field in ("session", "state", "state_revision"):
            if field in reply:
                known[field] = reply[field]
        known["last_status"] = reply.get("status")

    try:
        source = source_from_stdin(deadline)
        options = {key: value for key, value in {
            "max_steps": args.max_steps, "max_wait_ms": args.max_wait_ms,
        }.items() if value is not None}
        if args.dry_run:
            options["dry_run"] = True
        submit = message("observe" if args.observe else "script", source=source,
                         base=args.base, options=options, request_id=args.request_id)
        if args.expect_revision is not None:
            submit["expected_revision"] = args.expect_revision
        connection = Connection(args.socket, deadline)
        known["submission_attempted"] = True
        reply = connection.exchange(submit)
        remember(reply)
        if not reply["ok"]:
            emit(reply)
            return 1
        if reply.get("status") != "accepted" or type(reply.get("job_id")) is not int or reply["job_id"] < 1:
            raise ValueError("submission response has no accepted job ID")
        known["job_id"] = reply["job_id"]
        while True:
            reply = connection.exchange(message("result", job_id=known["job_id"]))
            if not reply["ok"]:
                emit(reply)
                return 1
            if reply.get("job_id") != known["job_id"]:
                raise ValueError("result job ID mismatch")
            remember(reply)
            if reply.get("status") in ("completed", "failed", "cancelled"):
                emit(reply)
                return 0 if reply["status"] == "completed" else 1
            if reply.get("status") != "running":
                raise ValueError("unexpected result status")
            time.sleep(min(args.poll_ms / 1000, remaining(deadline)))
    except KeyboardInterrupt:
        cancel_reply = None
        note = "No confirmed active job to cancel; query session status."
        if connection is not None:
            connection.close()
        if known["job_id"] is not None and known["last_status"] in ("accepted", "running"):
            control = None
            try:
                control = Connection(args.socket, time.monotonic() + 2, connection.identity)
                cancel_reply = control.exchange(message("cancel", expected_revision=known["state_revision"]))
                note = "Query this job's result to confirm cleanup; cancellation is not completion."
            except (OSError, ValueError, EOFError, RecursionError, KeyboardInterrupt) as error:
                note = f"Cancellation was not confirmed: {error}"
            finally:
                if control is not None:
                    control.close()
        emit({"client_status": "interrupted", **known, "cleanup_complete": False,
              "cancel_response": cancel_reply, "message": note})
        return 130
    except TimeoutError as error:
        emit({"client_status": "timeout", **known, "cleanup_complete": False,
              "message": str(error) or "client deadline expired; source was not retried"})
        return 124
    except (OSError, ValueError, EOFError, RecursionError) as error:
        emit({"client_status": "error", **known, "cleanup_complete": False, "message": str(error)})
        return 1
    finally:
        if connection is not None:
            connection.close()


if __name__ == "__main__":
    raise SystemExit(main())
