"""A process node for the tests, in plain Python with no packages.

It speaks the protocol directly, as a program in any language would. What it
does on each step depends on its `mode` parameter.
"""

import base64
import json
import os
import socket
import sys
import time

PREFIX = "@ruro "


def channel():
    addr = os.environ.get("RURO_NODE_ADDR")
    if not addr:
        return sys.stdin, sys.stdout
    host, port = addr.rsplit(":", 1)
    stream = socket.create_connection((host, int(port))).makefile("rw")
    return stream, stream


def send(out, obj):
    out.write(PREFIX + json.dumps(obj) + "\n")
    out.flush()


def invert(img):
    data = bytes(255 - b for b in base64.b64decode(img["data"]))
    return dict(img, data=base64.b64encode(data).decode())


def main():
    inp, out = channel()
    # Output that is not protocol is logged by the host, not parsed.
    print("node.py starting; this line is not protocol", flush=True)
    hello = {"ruro": int(os.environ.get("TEST_PROTOCOL", "1"))}
    if "RURO_NODE_TOKEN" in os.environ and os.environ.get("RURO_NODE_ADDR"):
        hello["token"] = os.environ["RURO_NODE_TOKEN"]
    send(out, hello)

    params = {}
    estops = 0
    for line in inp:
        if not line.startswith(PREFIX):
            continue
        req = json.loads(line[len(PREFIX):])
        op = req["op"]
        if op == "start":
            params = req["params"]
            if params.get("mode") == "refuse":
                send(out, {"error": "refusing to start"})
                continue
            send(out, {})
        elif op == "param":
            if req["name"] == "locked":
                send(out, {"error": "locked cannot change"})
            else:
                params[req["name"]] = req["value"]
                send(out, {})
        elif op == "estop":
            estops += 1
            send(out, {})
        elif op == "stop":
            send(out, {})
            return
        elif op == "step":
            mode = params.get("mode", "scale")
            mark = os.environ.get("TEST_MARK")
            if mode == "crash_once" and mark and not os.path.exists(mark):
                open(mark, "w").close()
                os._exit(3)
            if mode == "hang":
                time.sleep(3600)
            if mode == "garble":
                out.write(PREFIX + "{not json\n")
                out.flush()
                continue
            if req["steps"] == 1:
                print("warning: first step", file=sys.stderr, flush=True)
            outputs = {
                "output": [m["msg"] * params["k"] for m in req["inputs"].get("input", [])],
                "info": [
                    "pid=%d estops=%d estopped=%s mode=%s"
                    % (os.getpid(), estops, req["estopped"], mode)
                ],
            }
            if "image" in req["inputs"]:
                images = [m["msg"] for m in req["inputs"]["image"]]
                outputs["inverted"] = images if mode == "echo" else [invert(m) for m in images]
            reply = {"outputs": outputs}
            if mode == "complain":
                reply["error"] = "something is off"
            send(out, reply)


main()
