"""End-to-end check of Rakumux's Droid integration, offline.

A throwaway daemon (`RAKUMUX_HOME`, `HOME` and `FACTORY_HOME_OVERRIDE` all in
a temporary directory) runs `fake_droid.py` as `droid` in a session, with the
hooks `rakumux session hook-config --agent droid` prints installed, and is
driven over its socket as the account service and the TUI drive it:
lifecycle (working, blocked, done, idle), `agent_output`, `user_prompt`,
`agent_turn`, `session_turn`, `session_transcript`, reply capture, places, a
subagent counted, `session_submit` typed in as a new turn, the resume
command, and revival after a crash. It never touches the real `~/.rakumux` or
`~/.factory`, starts no real agent, and needs no network or account.

    cd tui && cargo build --workspace --locked
    python3 -I ../integrations/droid/e2e/run.py   # RAKUMUX_TARGET=<dir> for other builds
"""

import json
import os
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
TARGET = os.environ.get("RAKUMUX_TARGET") or os.path.join(ROOT, "tui", "target", "debug")
TUI = os.path.join(TARGET, "tui")
PTYD = os.path.join(TARGET, "rakumux-ptyd")
PROMPT = "Fix the flaky login test"


class Failure(Exception):
    pass


def check(condition, what, detail=None):
    if not condition:
        raise Failure("%s%s" % (what, "" if detail is None else ": %r" % (detail,)))
    print("ok   " + what)


class Daemon:
    """A client of the daemon's own protocol: `[u32 len][u8 kind][json]` frames."""

    def __init__(self, home):
        with open(os.path.join(home, "run", "service.json")) as f:
            registration = json.load(f)
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(registration["socket"])
        hello = self.request({"op": "hello", "protocol": 1, "client": "droid-e2e",
                              "version": "e2e",
                              "auth": {"kind": "user", "token": registration["token"]}})
        assert hello["type"] == "hello", hello

    def request(self, message):
        payload = json.dumps(message).encode()
        self.sock.sendall(struct.pack(">IB", len(payload) + 1, 0) + payload)
        while True:
            length, kind = struct.unpack(">IB", self.recv(5))
            body = self.recv(length - 1)
            if kind == 0:
                return json.loads(body)

    def recv(self, count):
        data = b""
        while len(data) < count:
            chunk = self.sock.recv(count - len(data))
            if not chunk:
                raise Failure("the daemon closed the connection")
            data += chunk
        return data

    def ok(self, message):
        answer = self.request(message)
        if answer.get("type") == "error":
            raise Failure("%s: %s" % (message["op"], answer["error"]))
        return answer

    def view(self, sid):
        return self.ok({"op": "session_get", "id": sid})["session"]

    def screen(self, sid):
        return self.ok({"op": "session_read", "id": sid, "source": "recent_unwrapped"})["text"]

    def wait(self, sid, what, test, timeout=20.0):
        deadline = time.time() + timeout
        view = None
        while time.time() < deadline:
            view = self.view(sid)
            if test(view):
                print("ok   " + what)
                return view
            time.sleep(0.1)
        raise Failure("never %s; view: %s\nscreen:\n%s" % (
            what, json.dumps(view, indent=1)[:3000], self.screen(sid)))


def body_offset(path, needle):
    text = open(path, "rb").read()
    head = text.index(b"\n") + 1
    at = text.index(needle.encode())
    return text.rindex(b"\n", 0, at) + 1 - head


def main():
    for binary in (TUI, PTYD):
        if not os.access(binary, os.X_OK):
            sys.exit("build first (cd tui && cargo build --workspace): %s" % binary)
    tmp = tempfile.mkdtemp(prefix="rakumux-droid-e2e-")
    dirs = {name: os.path.join(tmp, name) for name in ("home", "rakumux", "factory", "bin", "work")}
    for path in dirs.values():
        os.makedirs(path)
    factory = os.path.join(dirs["factory"], ".factory")
    os.makedirs(factory)
    work = os.path.realpath(dirs["work"])
    os.makedirs(os.path.join(work, ".git"))
    with open(os.path.join(work, ".git", "HEAD"), "w") as f:
        f.write("ref: refs/heads/main\n")
    with open(os.path.join(work, "README.md"), "w") as f:
        f.write("# demo\n")
    droid = os.path.join(dirs["bin"], "droid")
    with open(os.path.join(HERE, "fake_droid.py")) as src, open(droid, "w") as dst:
        dst.write("#!/usr/bin/env python3\n" + src.read())
    os.chmod(droid, 0o755)
    env = {k: v for k, v in os.environ.items() if not k.startswith("RAKUMUX_")}
    env.update({
        "PATH": dirs["bin"] + ":" + os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": dirs["home"],
        "RAKUMUX_HOME": dirs["rakumux"],
        "RAKUMUX_PTYD_BIN": PTYD,
        "RAKUMUX_BIN": TUI,
        "FACTORY_HOME_OVERRIDE": dirs["factory"],
        "FACTORY_DROID_AUTO_UPDATE_ENABLED": "false",
        "SHELL": "/bin/bash",
        "LANG": "C.UTF-8",
        "PS1": "$ ",
    })
    env.pop("FACTORY_SESSION_ID", None)
    hooks = subprocess.run([TUI, "session", "hook-config", "--agent", "droid"],
                           env=env, capture_output=True, text=True, check=True).stdout
    with open(os.path.join(factory, "hooks.json"), "w") as f:
        f.write(hooks)
    daemon = None
    try:
        subprocess.run([TUI, "daemon", "start"], env=env, check=True,
                       stdout=subprocess.DEVNULL, cwd=work)
        d = Daemon(dirs["rakumux"])
        # A client inside an earlier Droid hands its session id to new sessions.
        session_env = sorted(env.items()) + [("FACTORY_SESSION_ID", "leaked")]
        created = d.ok({"op": "session_create", "scope": work, "name": "droid",
                        "argv": ["/bin/bash", "--noprofile", "--norc"], "env": session_env,
                        "cols": 100, "rows": 30})["session"]
        sid = created["id"]
        d.wait(sid, "the shell is up", lambda v: v.get("terminal"))
        d.ok({"op": "session_submit", "id": sid, "text": "echo FSI=${FACTORY_SESSION_ID:-none}"})
        deadline = time.time() + 10
        while "FSI=none" not in d.screen(sid) and time.time() < deadline:
            time.sleep(0.1)
        check("FSI=none" in d.screen(sid), "FACTORY_SESSION_ID never reaches a new session")
        d.ok({"op": "reply_capture", "enabled": True})
        d.ok({"op": "session_submit", "id": sid, "text": "droid --auto high"})
        view = d.wait(sid, "detected as droid, idle at its prompt",
                      lambda v: (v.get("agent") or {}).get("kind") == "droid"
                      and v.get("lifecycle") == "idle")
        foreground = view["terminal"]["foreground_process"]

        # A prompt typed in as a reply is (paste, then Enter).
        d.ok({"op": "session_submit", "id": sid, "text": PROMPT,
              "expect_foreground": foreground})
        d.wait(sid, "working while its hooks say so",
               lambda v: v.get("lifecycle") == "working"
               and (v.get("activity") or {}).get("phase") in ("working", "running_tool"))
        view = d.wait(sid, "done when its Stop hook fires",
                      lambda v: v.get("lifecycle") == "done"
                      and v.get("agent_output") == "Done: " + PROMPT)
        conversation = view["agent"]["agent_session_id"]
        session_file = None
        for root, _, files in os.walk(os.path.join(factory, "sessions")):
            if conversation + ".jsonl" in files:
                session_file = os.path.join(root, conversation + ".jsonl")
        check(session_file is not None, "the hooks named Droid's conversation", conversation)
        check(view.get("user_prompt") == PROMPT, "user_prompt", view.get("user_prompt"))
        turn = view.get("agent_turn") or {}
        check(turn.get("conversation_id") == conversation, "agent_turn names the conversation", turn)
        content = d.ok({"op": "session_turn", "id": sid})["turn"]
        check(content.get("prompt") == PROMPT, "session_turn's prompt", content)
        check(content["messages"][-1] == "Done: " + PROMPT and
              "Create: src/turn1.txt" in content["messages"], "session_turn's messages",
              content["messages"])
        replies = []
        deadline = time.time() + 15
        while time.time() < deadline:
            replies = d.ok({"op": "reply_list"})["replies"]
            if replies:
                break
            time.sleep(0.2)
        check(replies and replies[0]["agent"] == "droid"
              and replies[0]["text"] == "Done: " + PROMPT
              and replies[0].get("conversation_id") == conversation,
              "reply capture: agent droid, its text, its conversation", replies)
        first_reply = replies[0]
        page = d.ok({"op": "session_transcript", "id": sid})["transcript"]
        roles = [m["role"] for m in page["messages"]]
        check(page["agent"] == "droid" and page["conversation_id"] == conversation,
              "session_transcript names droid and the conversation")
        check(roles == ["user", "assistant", "assistant", "tool", "assistant", "tool",
                        "assistant"], "transcript roles, no hook rows", roles)
        final = page["messages"][-1]
        check(first_reply.get("position") == final["position"]
              == body_offset(session_file, final["id"]),
              "the reply's position is its message's, past the rewritten first line",
              (first_reply.get("position"), final["position"]))
        with open(session_file) as f:
            title = json.loads(f.readline())["title"]
        check(title == PROMPT[:40], "Droid renamed the session (its first line rewritten)", title)
        view = d.wait(sid, "places: the repository it changed",
                      lambda v: any(p["path"] == work and p.get("edited")
                                    for p in v.get("places") or []))

        # An approval waits for the user.
        d.ok({"op": "session_submit", "id": sid, "text": "approve the deploy",
              "expect_foreground": foreground})
        d.wait(sid, "blocked on its approval", lambda v: v.get("lifecycle") == "blocked"
               and (v.get("activity") or {}).get("phase") == "needs_input")
        d.ok({"op": "session_send", "id": sid, "text": "y"})
        d.wait(sid, "done after the approval",
               lambda v: v.get("lifecycle") == "done"
               and v.get("agent_output") == "Done: approve the deploy")
        d.ok({"op": "session_touch", "id": sid})
        d.wait(sid, "idle once read", lambda v: v.get("lifecycle") == "idle")

        # A Task subagent is counted, never the session's own conversation.
        d.ok({"op": "session_submit", "id": sid, "text": "delegate the docs",
              "expect_foreground": foreground})
        view = d.wait(sid, "a subagent counted",
                      lambda v: (v.get("activity") or {}).get("active_subagents") == 1)
        check(view["agent"]["agent_session_id"] == conversation,
              "the subagent's hooks don't take the session over")
        view = d.wait(sid, "the subagent done, the turn finished",
                      lambda v: v.get("lifecycle") == "done"
                      and (v.get("activity") or {}).get("active_subagents") == 0
                      and v.get("agent_output") == "Done: delegate the docs")
        check(view["agent_turn"]["conversation_id"] == conversation,
              "still following the root conversation")

        # A reply written back: typed in, then a new turn with its prompt.
        before = view["agent_turn"]["id"]
        d.ok({"op": "session_submit", "id": sid, "text": "Also check the web app",
              "quiet_ms": 10000, "expect_foreground": foreground})
        view = d.wait(sid, "the reply typed in shows up as a new turn",
                      lambda v: v.get("user_prompt") == "Also check the web app"
                      and (v.get("agent_turn") or {}).get("id") != before)
        d.wait(sid, "and is answered", lambda v: v.get("lifecycle") == "done"
               and v.get("agent_output") == "Done: Also check the web app")
        deadline = time.time() + 15
        while time.time() < deadline:
            replies = d.ok({"op": "reply_list"})["replies"]
            if any(r["text"] == "Done: Also check the web app" for r in replies):
                break
            time.sleep(0.2)
        check(any(r["text"] == "Done: Also check the web app" for r in replies),
              "its answer captured")
        later = d.ok({"op": "session_transcript", "id": sid, "after": page["cursor"]})["transcript"]
        check(later["messages"][0]["role"] == "user"
              and later["messages"][0]["parts"][0]["text"] == "approve the deploy",
              "the transcript reads on from its cursor, nothing twice",
              later["messages"][0])
        hook_context = os.path.join(factory, "hook-context.log")
        check(not os.path.exists(hook_context),
              "Droid never got a hook answer it would add as context")

        # Resume: the command it records, and revival after a crash.
        view = d.wait(sid, "a resume command for its conversation",
                      lambda v: (v.get("resume") or {}).get("source") == "known_agent")
        check(view["resume"]["argv"] == ["droid", "--auto", "high", "--resume", conversation],
              "droid --auto high --resume <id>", view["resume"]["argv"])
        with open(os.path.join(dirs["rakumux"], "run", "service.json")) as f:
            pid = json.load(f)["pid"]
        os.kill(pid, signal.SIGKILL)
        time.sleep(0.5)
        subprocess.run([TUI, "daemon", "start"], env=env, check=True,
                       stdout=subprocess.DEVNULL, cwd=work)
        d = Daemon(dirs["rakumux"])
        d.wait(sid, "revived after the crash", lambda v: v.get("status") == "live")
        deadline = time.time() + 15
        while time.time() < deadline and "RESUMED " + conversation not in d.screen(sid):
            time.sleep(0.2)
        check("RESUMED %s --auto high --resume %s" % (conversation, conversation)
              in d.screen(sid), "Droid resumed in its conversation")
        # The follower reads on the terminal's output, at most once a second:
        # Droid redraws as the user types.
        time.sleep(1.1)
        d.ok({"op": "session_send", "id": sid, "text": "x"})
        time.sleep(1.1)
        d.ok({"op": "session_send", "id": sid, "keys": ["backspace"]})
        d.wait(sid, "followed again after the revival",
               lambda v: (v.get("agent_turn") or {}).get("conversation_id") == conversation
               and v.get("user_prompt") == "Also check the web app")
        print("\nall checks passed")
    finally:
        subprocess.run([TUI, "daemon", "stop", "--yes"], env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if os.environ.get("KEEP"):
            print("kept", tmp)
        else:
            shutil.rmtree(tmp, ignore_errors=True)


if __name__ == "__main__":
    try:
        main()
    except Failure as failure:
        sys.exit("FAILED " + str(failure))
