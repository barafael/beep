#!/usr/bin/env python3
"""Takes screenshots of b33p's window, for the README and for comparing changes to the look.

It starts the mock (nothing is sent) with the agent harness, drives it the way a person would,
and saves the window at each step as a PNG. Runs offscreen: nothing opens on the desktop.

    cargo build -p b33p-slint --features harness
    python3 docs/screenshots/shoot.py --out target/shots/light
    python3 docs/screenshots/shoot.py --out target/shots/dark --scheme dark
    python3 docs/screenshots/shoot.py --out target/shots/fusion-dark --scheme dark --style fusion

The colour scheme comes from KDE's platform theme with a scheme of Breeze's (BreezeLight or
BreezeDark), so it needs KDE's Qt integration (plasma-integration); without it, Qt's defaults.
"""

import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCHEMES = Path("/usr/share/color-schemes")


class Agent:
    def __init__(self, path: Path):
        deadline = time.time() + 30
        while True:
            try:
                self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                self.sock.connect(str(path))
                break
            except OSError:
                if time.time() > deadline:
                    raise SystemExit(f"no agent socket at {path}")
                time.sleep(0.1)
        self.file = self.sock.makefile("rw", encoding="utf-8")
        self.next_id = 1

    def call(self, method: str, **params):
        request_id = self.next_id
        self.next_id += 1
        self.file.write(json.dumps({"id": request_id, "method": method, **params}) + "\n")
        self.file.flush()
        while True:
            line = self.file.readline()
            if not line:
                raise SystemExit("the app went away")
            answer = json.loads(line)
            if answer.get("id") == request_id:
                if "error" in answer:
                    raise SystemExit(f"{method} {params}: {answer['error']}")
                return answer["result"]

    def act(self, kind: str, **fields):
        return self.call("act", action={"type": kind, **fields})

    def snapshot(self):
        return self.call("snapshot")

    def wait_for(self, what: str, until, timeout: float = 20):
        deadline = time.time() + timeout
        while time.time() < deadline:
            snapshot = self.snapshot()
            if until(snapshot):
                return snapshot
            time.sleep(0.1)
        raise SystemExit(f"waited in vain for {what}")


def chat_id(snapshot, title: str) -> str:
    for chat in snapshot["conversations"]:
        if chat["title"] == title:
            return chat["id"]
    raise SystemExit(f"no chat called {title}")


def has_message(snapshot, text: str) -> bool:
    return any(m["text"] == text for m in snapshot["messages"])


def scratch_home(scheme: str) -> Path:
    home = Path(tempfile.mkdtemp(prefix="b33p-shots-"))
    for sub in ["config", "data", "cache", "runtime"]:
        (home / sub).mkdir()
    colors = SCHEMES / ("BreezeDark.colors" if scheme == "dark" else "BreezeLight.colors")
    if colors.exists():
        text = colors.read_text()
        name = "BreezeDark" if scheme == "dark" else "BreezeLight"
        (home / "config" / "kdeglobals").write_text(
            text + f"\n[General]\nColorScheme={name}\n\n[KDE]\nwidgetStyle=Breeze\n"
        )
    return home


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--binary", default=str(ROOT / "target/debug/b33p"))
    parser.add_argument("--out", required=True)
    parser.add_argument("--scheme", choices=["light", "dark"], default="light")
    parser.add_argument("--style", choices=["system", "fusion"], default="system")
    parser.add_argument("--only", help="comma-separated scene names")
    parser.add_argument("--width", type=float, default=1000)
    parser.add_argument("--height", type=float, default=680)
    args = parser.parse_args()

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    home = scratch_home(args.scheme)
    sock = home / "runtime" / "agent.sock"
    env = dict(os.environ)
    env.update(
        B33P_MOCK="1",
        B33P_AGENT_SOCKET=str(sock),
        XDG_CONFIG_HOME=str(home / "config"),
        XDG_DATA_HOME=str(home / "data"),
        XDG_CACHE_HOME=str(home / "cache"),
        XDG_RUNTIME_DIR=str(home / "runtime"),
        DBUS_SESSION_BUS_ADDRESS=f"unix:path={home / 'bus'}",
        QT_QPA_PLATFORM="offscreen",
        QT_QPA_PLATFORMTHEME="kde",
        LANG="en_US.UTF-8",
    )
    if args.style == "fusion":
        env["QT_STYLE_OVERRIDE"] = "Fusion"
    else:
        env.pop("QT_STYLE_OVERRIDE", None)
    app = subprocess.Popen(
        [args.binary], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
    )
    only = set(args.only.split(",")) if args.only else None
    try:
        agent = Agent(sock)
        # Still pictures: nothing caught halfway through fading in.
        agent.act("set_settings", animations="off", show_calendar=True, show_activity=False)
        agent.act("resize", width=args.width, height=args.height)
        chats = agent.wait_for(
            "the mock's chats",
            lambda s: s["screen"]["name"] == "chats"
            and {"Alice", "Bob"} <= {c["title"] for c in s["conversations"]},
        )
        time.sleep(1.0)

        def shot(name: str, settle: float = 0.5):
            if only is not None and name not in only:
                return
            time.sleep(settle)
            agent.call("screenshot", path=str((out / f"{name}.png").resolve()))
            print(name)

        alice = chat_id(chats, "Alice")
        shot("01-chats")

        agent.act("select", conversation=alice)
        agent.wait_for("Alice open", lambda s: s["selected"] == alice and s["messages"])
        shot("02-conversation", 1.0)

        agent.act("type", text="Hello from the harness! _So this is b33p._")
        shot("03-composing")

        agent.act("send")
        agent.wait_for(
            "the echo",
            lambda s: has_message(s, "You said: Hello from the harness! So this is b33p."),
        )
        time.sleep(1.5)
        shot("04-answered")

        agent.act("open_details")
        shot("05-details", 1.0)
        agent.act("close_details")

        agent.act("open_media")
        shot("06-media", 1.0)
        agent.act("close_media")

        agent.act("show", what="calendar", open=True)
        shot("07-calendar")
        agent.act("show", what="calendar", open=False)

        agent.act("open_chat_search")
        agent.act("search_chat", text="message")
        shot("08-chat-search", 1.0)
        agent.act("close_chat_search")

        snapshot = agent.snapshot()
        last = len(snapshot["messages"]) - 1
        agent.act("toggle_select", index=last)
        agent.act("toggle_select", index=last - 1)
        shot("09-selection")
        agent.act("clear_selection")

        agent.act("reply_to", index=last)
        shot("10-replying")
        agent.act("cancel_reply")

        group = chat_id(snapshot, "Rustaceans 🦀")
        agent.act("select", conversation=group)
        agent.wait_for("the group open", lambda s: s["selected"] == group and s["messages"])
        shot("11-group", 1.0)

        for chat in agent.snapshot()["conversations"]:
            if chat["request"]:
                agent.act("select", conversation=chat["id"])
                agent.wait_for("the request open", lambda s: s["selected"] == chat["id"])
                shot("12-request", 1.0)
                break

        agent.act("show", what="settings", open=True)
        shot("13-settings")
        agent.act("show", what="settings", open=False)

        agent.act("open_new_group")
        shot("14-new-group")
        agent.act("close_group_draft")

        agent.act("open_join_group", link="https://signal.group/#mock-approve")
        agent.wait_for("the link looked up", lambda s: s["join"] and s["join"]["title"])
        shot("15-join")
        agent.act("close_join_group")

        agent.act("select", conversation=alice)
        agent.wait_for("Alice open", lambda s: s["selected"] == alice and s["messages"])
        agent.act("forward", index=len(agent.snapshot()["messages"]) - 1)
        shot("16-forward")
        agent.act("cancel_forward")

        agent.act("show", what="unlink", open=True)
        shot("17-unlink")
        agent.act("show", what="unlink", open=False)

        agent.act("show", what="stories", open=True)
        shot("18-stories")
        agent.act("open_story", conversation=alice)
        shot("19-story", 1.0)
        agent.act("close_story")
        agent.act("show", what="stories", open=False)
    finally:
        app.terminate()
        try:
            app.wait(timeout=10)
        except subprocess.TimeoutExpired:
            app.kill()
        shutil.rmtree(home, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
