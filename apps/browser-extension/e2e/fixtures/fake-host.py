#!/usr/bin/env python3
"""A stand-in for `haarness browser native-host` in the extension's end-to-end tests.

Chrome starts it through the native messaging manifest the test writes into its
temporary profile. It relays Chrome's framed messages (4-byte little-endian
length + UTF-8 JSON) to the test over TCP as newline-delimited JSON, and the
test's lines back to Chrome, so the test plays the bridge and every server.

The test finds it through the environment Chrome passes down:
  HAARNESS_E2E_RELAY  host:port of the test's relay server
  HAARNESS_E2E_LOG    optional file for one JSON line per lifecycle event
A line {"__exit": <code>} from the test makes it exit, as a crashing bridge would.
"""
import json
import os
import socket
import struct
import sys
import threading
import time

LOG = os.environ.get("HAARNESS_E2E_LOG")
lock = threading.Lock()


def log(**event):
    if not LOG:
        return
    event = {"t": round(time.time(), 3), "pid": os.getpid(), **event}
    try:
        os.makedirs(os.path.dirname(LOG), exist_ok=True)
        with open(LOG, "a", encoding="utf-8") as file:
            file.write(json.dumps(event) + "\n")
    except OSError:
        pass


def read_frame():
    head = sys.stdin.buffer.read(4)
    if len(head) < 4:
        return None
    (length,) = struct.unpack("<I", head)
    body = b""
    while len(body) < length:
        part = sys.stdin.buffer.read(length - len(body))
        if not part:
            return None
        body += part
    return body


def write_frame(body):
    with lock:
        sys.stdout.buffer.write(struct.pack("<I", len(body)))
        sys.stdout.buffer.write(body)
        sys.stdout.buffer.flush()


def main():
    host, port = os.environ["HAARNESS_E2E_RELAY"].rsplit(":", 1)
    relay = socket.create_connection((host, int(port)))
    log(event="start", argv=sys.argv[1:])
    stream = relay.makefile("rwb")

    def from_chrome():
        while True:
            body = read_frame()
            if body is None:
                log(event="stdin-eof")
                try:
                    relay.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass
                os._exit(0)
            stream.write(body.replace(b"\n", b" ") + b"\n")
            stream.flush()

    threading.Thread(target=from_chrome, daemon=True).start()
    for line in stream:
        message = json.loads(line)
        if isinstance(message, dict) and "__exit" in message:
            log(event="exit", code=message["__exit"])
            os._exit(int(message["__exit"]))
        write_frame(json.dumps(message, ensure_ascii=False).encode("utf-8"))
    log(event="relay-closed")
    os._exit(0)


main()
