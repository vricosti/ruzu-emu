#!/usr/bin/env python3
"""Control an explicitly enabled Ruzu GUI input session, never the host keyboard."""
import argparse
import json
import socket
import sys
import tempfile
import time
from pathlib import Path


def request(directory, payload):
    with tempfile.TemporaryDirectory(prefix="client-", dir=directory) as client_dir:
        with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as client:
            reply_address = str(Path(client_dir) / "reply")
            # Darwin's returned sockaddr length otherwise excludes the NUL,
            # while Rust's Unix SocketAddr decoder removes that final byte.
            if sys.platform == "darwin":
                reply_address += "\0"
            client.bind(reply_address)
            client.settimeout(10)
            client.sendto(json.dumps(payload).encode(), str(directory / "control.sock"))
            response = json.loads(client.recv(16384))
    if not response.get("ok"):
        raise RuntimeError(response.get("error", "input command failed"))
    return response["result"]


def capture(directory, name):
    reply = request(directory, {"command": "capture", "name": name})
    path = Path(reply["queued"])
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if path.is_file():
            with path.open("rb") as image:
                if image.seek(0, 2) >= 12:
                    image.seek(-12, 2)
                    if image.read() == b"\x00\x00\x00\x00IEND\xaeB`\x82":
                        return str(path)
        time.sleep(0.1)
    raise TimeoutError(f"renderer did not complete screenshot: {path}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("session", type=Path)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("status")
    commands.add_parser("release")
    press = commands.add_parser("press")
    press.add_argument("buttons", nargs="+")
    press.add_argument("--hold-ms", type=int, default=300)
    shot = commands.add_parser("capture")
    shot.add_argument("name")
    args = parser.parse_args()
    if args.command == "capture":
        result = capture(args.session, args.name)
    elif args.command == "press":
        result = request(args.session, {"command": "press", "buttons": args.buttons,
                                        "hold_ms": args.hold_ms})
    else:
        result = request(args.session, {"command": args.command})
    print(json.dumps(result))


if __name__ == "__main__":
    main()
