#!/usr/bin/env python3
"""Control an explicitly enabled Ruzu GUI input session, never the host keyboard."""
import argparse
import json
import os
import socket
import sys
import tempfile
import time
from pathlib import Path

# Common::SettingsInput::NativeButton order, not keyboard codes.
BUTTONS = ["A", "B", "X", "Y", "LSTICK", "RSTICK", "L", "R", "ZL", "ZR",
           "PLUS", "MINUS", "LEFT", "UP", "RIGHT", "DOWN", "SL", "SR"]


def snapshot(status):
    if not status.get("has_session") or not status.get("program_id"):
        raise RuntimeError("Start the game before recording or replaying")
    buttons = status["buttons"]
    if len(buttons) != 22 or any(type(value) is not bool for value in buttons):
        raise ValueError("invalid controller button state")
    # Auxiliary system/right Joy-Con buttons are outside this replay protocol.
    # A screenshot press must not discard an otherwise useful menu recording.
    return [name for name, pressed in zip(BUTTONS, buttons) if pressed]


def record(directory, output, duration):
    if not 0 < duration <= 3600:
        raise ValueError("duration must be in (0, 3600] seconds")
    initial = request(directory, {"command": "status"})
    if snapshot(initial):
        raise ValueError("Release all buttons before recording")
    identity = (initial["pid"], initial["generation"], initial["program_id"])
    data = {"version": 1, "kind": "ruzu-gui-buttons", "complete": False,
            "program_id": initial["program_id"], "interval_ms": 20,
            "events": [{"at_us": 0, "buttons": []}]}
    # Never overwrite an existing recording, even on a failed attempt.
    with os.fdopen(os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as file:
        start = previous = time.monotonic()
        current = []
        print("Recording guest buttons. Navigate now; release buttons before Ctrl-C to finish.",
              file=sys.stderr, flush=True)
        try:
            try:
                while time.monotonic() - start < duration:
                    time.sleep(0.02)
                    status = request(directory, {"command": "status"})
                    now = time.monotonic()
                    if now - previous > 0.25:
                        raise RuntimeError("recording gap exceeded 250 ms; retry from the same menu")
                    previous = now
                    if (status["pid"], status["generation"], status["program_id"]) != identity:
                        raise RuntimeError("emulation session changed during recording")
                    buttons = snapshot(status)
                    if buttons != current:
                        current = buttons
                        data["events"].append({"at_us": round((now-start)*1e6), "buttons": buttons})
            except KeyboardInterrupt:
                pass
            if current:
                raise RuntimeError("recording ended with held buttons; recording is incomplete")
            data["complete"] = True
        finally:
            data["duration_us"] = round((time.monotonic()-start)*1e6)
            json.dump(data, file, indent=2)
            file.write("\n")
    return {"saved": str(output), "events": len(data["events"])}


def validate_recording(data):
    if (not isinstance(data, dict) or data.get("version") != 1 or data.get("kind") != "ruzu-gui-buttons"
            or data.get("complete") is not True):
        raise ValueError("unsupported or incomplete recording")
    duration = data.get("duration_us")
    if type(duration) is not int or not 0 < duration <= 3_601_000_000:
        raise ValueError("invalid recording duration")
    if type(data.get("program_id")) is not int or data["program_id"] <= 0:
        raise ValueError("invalid program ID")
    events = data.get("events")
    if not isinstance(events, list) or not 1 <= len(events) <= 180001:
        raise ValueError("invalid events")
    previous = -1
    for event in events:
        if not isinstance(event, dict):
            raise ValueError("invalid event")
        at = event.get("at_us")
        buttons = event.get("buttons")
        if type(at) is not int or not previous < at <= duration:
            raise ValueError("event times must be strictly increasing within duration")
        if (not isinstance(buttons, list) or len(buttons) > len(BUTTONS)
                or any(not isinstance(name, str) or name not in BUTTONS for name in buttons)
                or len(set(buttons)) != len(buttons)):
            raise ValueError("invalid buttons")
        previous = at
    if events[0] != {"at_us": 0, "buttons": []} or events[-1]["buttons"]:
        raise ValueError("recording must start and end with released buttons")
    return events


def replay(directory, source):
    data = json.loads(source.read_text())
    events = validate_recording(data)
    initial = request(directory, {"command": "status"})
    if snapshot(initial) or initial["program_id"] != data["program_id"]:
        raise ValueError("wrong game or buttons already held")
    identity = (initial["pid"], initial["generation"], initial["program_id"])
    start = time.monotonic()
    current = []
    index = 0
    try:
        while index < len(events):
            status = request(directory, {"command": "status"})
            if (status.get("pid"), status.get("generation"), status.get("program_id")) != identity:
                raise RuntimeError("emulation session changed during replay")
            now = time.monotonic()
            deadline = start + events[index]["at_us"] / 1e6
            if now >= deadline:
                if now - deadline > 0.25:
                    raise RuntimeError("replay late by more than 250 ms; stopped")
                current = events[index]["buttons"]
                index += 1
            # Heartbeat refreshes the GUI watchdog for long holds, without new edges.
            request(directory, {"command": "set_buttons", "buttons": current, "hold_ms": 1000})
            if index < len(events):
                time.sleep(max(0, min(0.25, start + events[index]["at_us"]/1e6 - time.monotonic())))
    finally:
        try:
            request(directory, {"command": "release"})
        except Exception as error:
            print(f"Release request failed; GUI watchdog will release held buttons: {error}", file=sys.stderr)
    return {"replayed": str(source), "events": len(events)}


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
    recording = commands.add_parser("record")
    recording.add_argument("file", type=Path)
    recording.add_argument("--duration", type=float, default=180)
    playback = commands.add_parser("replay")
    playback.add_argument("file", type=Path)
    args = parser.parse_args()
    if args.command == "record":
        result = record(args.session, args.file, args.duration)
    elif args.command == "replay":
        result = replay(args.session, args.file)
    elif args.command == "capture":
        result = capture(args.session, args.name)
    elif args.command == "press":
        result = request(args.session, {"command": "press", "buttons": args.buttons,
                                        "hold_ms": args.hold_ms})
    else:
        result = request(args.session, {"command": args.command})
    print(json.dumps(result))


if __name__ == "__main__":
    main()
