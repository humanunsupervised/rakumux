"""A stand-in for Droid (Factory's CLI) for Rakumux's end-to-end check.

Real model turns need a Factory sign-in, so this plays Droid's part offline,
as Droid 1.0.0 does it (formats read from its bundle):

- its session file, `$FACTORY_HOME_OVERRIDE/.factory/sessions/<folder key>/<id>.jsonl`:
  a `session_start` first line that it rewrites in place when it names the
  session, then one `{"type":"message",…}` line per message, its hook rows
  among them;
- its command hooks from `.factory/hooks.json` (run with `sh -c`, the JSON
  input on stdin and its string fields in the environment), whose stdout must
  be one of its hook-control objects or it would become context;
- its terminal: the title `⛬ <title>`, bracketed paste, the busy line, the
  approval panel and the autonomy footer, redrawn in place;
- `--resume <id>` and the flags Rakumux keeps.

Installed as `droid` on PATH (a Python script named `droid`), so Rakumux sees
`python3 … /bin/droid`, the way it sees Droid's npm shim under node. Never a
real model, network or account.
"""

import json
import os
import re
import select
import subprocess
import sys
import termios
import time
import tty
import uuid

HOME = os.path.join(
    os.environ.get("FACTORY_HOME_OVERRIDE") or os.path.expanduser("~"), ".factory"
)
CWD = os.path.realpath(os.getcwd())
CONTEXT_LOG = os.path.join(HOME, "hook-context.log")
WIDTH = 78
SPINNER = "⠙"


def key(path):
    return "-" + re.sub(r"/+", "-", path.rstrip("/").lstrip("/"))


def now():
    t = time.time()
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(t)) + ".%03dZ" % (
        int(t * 1000) % 1000
    )


def compact(value):
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


class Session:
    def __init__(self, resume=None):
        self.id = resume
        self.path = None
        self.title = "New session"
        self.last = None
        self.turns = 0
        if resume:
            folder = os.path.join(HOME, "sessions")
            for root, _, files in os.walk(folder):
                if resume + ".jsonl" in files:
                    self.path = os.path.join(root, resume + ".jsonl")
            if self.path:
                with open(self.path) as f:
                    first = json.loads(f.readline())
                self.title = first.get("title", self.title)

    def create(self):
        self.id = str(uuid.uuid4())
        folder = os.path.join(HOME, "sessions", key(CWD))
        os.makedirs(folder, exist_ok=True)
        self.path = os.path.join(folder, self.id + ".jsonl")
        with open(self.path, "w") as f:
            f.write(compact(self.summary()) + "\n")

    def summary(self):
        return {
            "type": "session_start",
            "id": self.id,
            "title": self.title,
            "owner": "e2e",
            "version": 2,
            "cwd": CWD,
        }

    def append(self, line):
        with open(self.path, "a") as f:
            f.write(compact(line) + "\n")

    def message(self, role, content, **extra):
        mid = str(uuid.uuid4())
        message = {"role": role, "content": content}
        message.update(extra)
        line = {"type": "message", "id": mid, "timestamp": now(), "message": message}
        if self.last:
            line["parentId"] = self.last
        self.append(line)
        self.last = mid
        return mid

    def retitle(self, title):
        # Droid's writeSessionSummaryLine: the whole file read, its first line
        # replaced, written again in place (truncate, then write).
        self.title = title
        with open(self.path) as f:
            lines = f.read().split("\n")
        lines[0] = compact(self.summary())
        with open(self.path, "w") as f:
            f.write("\n".join(lines))


def hooks():
    try:
        with open(os.path.join(HOME, "hooks.json")) as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


HOOKS = hooks()


def run_hooks(session, event, **fields):
    data = {
        "session_id": session.id or "unknown",
        "transcript_path": session.path or "",
        "cwd": CWD,
        "permission_mode": "auto-high",
        "hook_event_name": event,
    }
    data.update(fields)
    env = dict(os.environ)
    env.update({k: str(v) for k, v in data.items() if isinstance(v, (str, int, bool))})
    env.update(FACTORY_PROJECT_DIR=CWD, DROID_PROJECT_DIR=CWD, CLAUDE_PROJECT_DIR=CWD)
    results = []
    commands = []
    for group in HOOKS.get(event, []):
        matcher = group.get("matcher")
        tool = fields.get("tool_name")
        if matcher not in (None, "", "*") and tool is not None and matcher != tool:
            continue
        for hook in group.get("hooks", []):
            commands.append(hook["command"])
            done = subprocess.run(
                ["sh", "-c", hook["command"]],
                input=compact(data),
                capture_output=True,
                text=True,
                env=env,
                timeout=hook.get("timeout", 60),
            )
            result = {"exitCode": done.returncode, "stdout": done.stdout, "stderr": ""}
            out = done.stdout.strip()
            control = False
            if out.startswith("{"):
                try:
                    parsed = json.loads(out)
                    keys = {"continue", "stopReason", "suppressOutput", "systemMessage",
                            "decision", "reason", "hookSpecificOutput"}
                    control = isinstance(parsed, dict) and bool(keys & set(parsed))
                    if isinstance(parsed, dict) and isinstance(parsed.get("suppressOutput"), bool):
                        result["suppressOutput"] = parsed["suppressOutput"]
                except ValueError:
                    pass
            if out and not control:
                # Droid would add this to the conversation as context.
                with open(CONTEXT_LOG, "a") as log:
                    log.write("%s %s\n" % (event, out))
            results.append(result)
    if commands and session.path:
        session.message(
            "user",
            [],
            visibility="user_only",
            hookEventName=event,
            hookCommands=commands,
            hookStatus="completed",
            hookResults=results,
        )


class Screen:
    def __init__(self):
        self.out = sys.stdout

    def write(self, text):
        self.out.write(text)
        self.out.flush()

    def title(self, text):
        self.write("\x1b]0;⛬ %s\x07" % text)

    def draw(self, lines, footer=True, typed=""):
        body = "\x1b[2J\x1b[H" + "\r\n".join(lines) + "\r\n\r\n"
        if footer:
            body += "╭" + "─" * WIDTH + "╮\r\n"
            body += ("│ > " + typed).ljust(WIDTH + 1) + "│\r\n"
            body += "╰" + "─" * WIDTH + "╯\r\n"
            body += "Auto (High) · allow all commands"
        self.write(body)


def main():
    args = sys.argv[1:]
    resume = None
    if "--resume" in args:
        at = args.index("--resume")
        resume = args[at + 1] if at + 1 < len(args) else None
    session = Session(resume)
    screen = Screen()
    screen.write("\x1b[?2004h\x1b[?1004h")
    screen.title(session.title if resume else "Droid")
    history = []
    if resume:
        history.append("RESUMED %s %s" % (resume, " ".join(args)))
        run_hooks(session, "SessionStart", source="resume")
    screen.draw(history)

    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    tty.setraw(fd)
    typed = ""
    pending = b""
    in_paste = False
    try:
        while True:
            ready, _, _ = select.select([fd], [], [], 0.5)
            if not ready:
                continue
            chunk = os.read(fd, 4096)
            if not chunk:
                return
            pending += chunk
            while pending:
                if in_paste:
                    end = pending.find(b"\x1b[201~")
                    if end < 0:
                        typed += pending.decode("utf-8", "replace")
                        pending = b""
                        break
                    typed += pending[:end].decode("utf-8", "replace")
                    pending = pending[end + 6:]
                    in_paste = False
                    screen.draw(history, typed=typed.split("\n")[0])
                    continue
                if pending.startswith(b"\x1b[200~"):
                    in_paste = True
                    pending = pending[6:]
                    continue
                if pending[:1] == b"\x1b":
                    # Another escape sequence (focus reports, arrows): passed over.
                    match = re.match(rb"\x1b\[[0-9;?]*[A-Za-z~]", pending)
                    pending = pending[match.end():] if match else pending[1:]
                    continue
                byte, pending = pending[:1], pending[1:]
                if byte in (b"\x03", b"\x04"):
                    run_hooks(session, "SessionEnd", reason="exit")
                    return
                if byte in (b"\r", b"\n"):
                    prompt = typed.strip()
                    typed = ""
                    if prompt:
                        turn(session, screen, history, prompt, fd)
                    screen.draw(history)
                    continue
                if byte == b"\x7f":
                    typed = typed[:-1]
                else:
                    typed += byte.decode("utf-8", "replace")
                screen.draw(history, typed=typed)
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)


def busy(screen, history, status):
    screen.draw(history + ["", "%s %s...  (Press ESC to stop)" % (SPINNER, status)])


def wait_key(fd, keys):
    while True:
        ready, _, _ = select.select([fd], [], [], 0.5)
        if ready and any(k in os.read(fd, 64) for k in keys):
            return


def call(session, name, tool_input, output):
    """One tool call of the agent's: its hooks, its message and its result."""
    tool_id = "call_" + uuid.uuid4().hex[:12]
    run_hooks(session, "PreToolUse", tool_name=name, tool_input=tool_input)
    session.message(
        "assistant",
        [{"type": "tool_use", "id": tool_id, "name": name, "input": tool_input}],
    )
    session.message(
        "user", [{"type": "tool_result", "tool_use_id": tool_id, "content": output}]
    )
    run_hooks(session, "PostToolUse", tool_name=name, tool_input=tool_input,
              tool_response={"success": True})


def turn(session, screen, history, prompt, fd):
    if not session.path:
        session.create()
        run_hooks(session, "SessionStart", source="startup")
    session.turns += 1
    history.append("> " + prompt)
    prompt_id = session.message("user", [{"type": "text", "text": prompt}],
                                userMessageSource="cli_tui")
    run_hooks(session, "UserPromptSubmit", prompt=prompt, has_images=False,
              message_id=prompt_id)
    busy(screen, history, "Thinking")
    time.sleep(1.0)
    session.message(
        "assistant",
        [
            {"type": "thinking", "thinking": "Planning: " + prompt, "signature": "e2e"},
            {"type": "text", "text": "Working on it."},
        ],
    )
    if "approve" in prompt:
        screen.draw(history + [
            "", "%s Waiting for tool confirmation..." % SPINNER, "",
            "Command to approve medium risk", "Run: ./deploy.sh", "",
            "> Yes, allow", "  Yes, and always allow medium", "  No, cancel", "",
            "↑↓ navigate · Enter select · Esc cancel",
            "Alt/Option+E to inspect approval details",
        ], footer=False)
        run_hooks(session, "PreToolUse", tool_name="Execute",
                  tool_input={"command": "./deploy.sh", "summary": "Deploy"})
        run_hooks(session, "Notification", notification_type="permission_prompt",
                  message="Droid needs your permission to use Execute")
        wait_key(fd, [b"y", b"\r"])
        busy(screen, history, "Executing")
        run_hooks(session, "PostToolUse", tool_name="Execute",
                  tool_input={"command": "./deploy.sh"}, tool_response={})
        time.sleep(0.5)
    elif "delegate" in prompt:
        busy(screen, history, "Invoking tools")
        child = Session()
        child.id = str(uuid.uuid4())
        child.path = os.path.join(os.path.dirname(session.path), child.id + ".jsonl")
        task_id = "call_task"
        with open(child.path, "w") as f:
            f.write(compact({"type": "session_start", "id": child.id, "title": "docs",
                             "owner": "e2e", "version": 2, "cwd": CWD,
                             "callingSessionId": session.id,
                             "callingToolUseId": task_id}) + "\n")
        run_hooks(session, "PreToolUse", tool_name="Task",
                  tool_input={"subagent_type": "worker", "description": "Write docs",
                              "prompt": "Write the docs"})
        run_hooks(child, "SessionStart", source="startup",
                  calling_session_id=session.id)
        child.message("user", [{"type": "text", "text": "Write the docs"}])
        run_hooks(child, "PreToolUse", tool_name="Read",
                  tool_input={"file_path": os.path.join(CWD, "README.md")})
        time.sleep(3.0)
        run_hooks(child, "PostToolUse", tool_name="Read", tool_input={})
        child.message("assistant", [{"type": "text", "text": "Docs written."}])
        run_hooks(child, "Stop", stop_hook_active=False, message_id=child.last)
        run_hooks(session, "SubagentStop", task_name="Write docs",
                  task_result="Docs written.")
        run_hooks(session, "PostToolUse", tool_name="Task", tool_input={})
    else:
        busy(screen, history, "Executing")
        target = os.path.join(CWD, "src", "turn%d.txt" % session.turns)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "w") as f:
            f.write(prompt + "\n")
        call(session, "Create", {"file_path": target, "content": prompt},
             "Created " + target)
        call(session, "Execute", {"command": "cd src && ls", "summary": "List the sources"},
             "turn1.txt")
    if session.turns == 1:
        # The automatic title after the first message: line 1 rewritten.
        session.retitle(prompt[:40])
        screen.title(session.title)
    time.sleep(0.5)
    reply = "Done: " + prompt
    final = session.message("assistant", [{"type": "text", "text": reply}])
    history.append("⛬ " + reply)
    run_hooks(session, "Stop", stop_hook_active=False, tool_execution_count=2,
              elapsed_time=2, message_id=final)
    session.append({"type": "agent_turn_outcome", "turnId": prompt_id,
                    "reason": "completed", "resultKind": "null"})


if __name__ == "__main__":
    main()
