#!/usr/bin/env python3
"""harness — a minimal coding agent loop for the Kimi Code subscription API.

Four parts:
  1. load_token()  — KIMI_API_KEY env var, or reuse the Kimi Code CLI's OAuth token.
  2. TOOLS + dispatch — read_file, write_file, edit_file, run_command.
  3. run_loop()    — call the model, execute tool calls, append results, repeat
                     until the model answers with plain text.
  4. main()        — a REPL (or one-shot --prompt) around the loop.

Stdlib only. No streaming, no compaction, no sandbox — see README.md.
"""

import argparse
import json
import os
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

BASE_URL = os.environ.get("KIMI_BASE_URL", "https://api.kimi.com/coding/v1")
MODEL = os.environ.get("KIMI_MODEL", "k3")
MAX_TURNS = 40            # tool-call round trips per user message
CMD_TIMEOUT = 120         # seconds for run_command
OUT_CAP = 12_000          # chars returned from run_command
READ_CAP = 2_000          # lines returned from read_file

YOLO = True               # yolo mode: skip run_command confirmation


# ---------------------------------------------------------------- auth

def load_token():
    """KIMI_API_KEY wins; otherwise reuse the Kimi Code CLI's stored OAuth token."""
    key = os.environ.get("KIMI_API_KEY")
    if key:
        return key
    cred = Path(os.environ.get("KIMI_CODE_HOME", Path.home() / ".kimi-code")) / "credentials" / "kimi-code.json"
    if cred.exists():
        try:
            data = json.loads(cred.read_text())
            if data.get("expires_at", 0) > time.time() + 60:
                return data["access_token"]
            err("OAuth token expired — run `kimi` once to refresh it, or set KIMI_API_KEY")
        except (json.JSONDecodeError, KeyError) as e:
            err(f"could not parse {cred}: {e}")
    sys.exit(
        "No credentials. Either:\n"
        "  - create an API key in the Kimi Code Console and `export KIMI_API_KEY=...`, or\n"
        "  - log in with the Kimi Code CLI (`kimi`, then /login) and rerun."
    )


# ---------------------------------------------------------------- tools

def tool_read_file(path, offset=1, limit=None):
    p = Path(path).expanduser()
    lines = p.read_text(errors="replace").splitlines()
    start = max(int(offset) - 1, 0)
    end = start + min(int(limit), READ_CAP) if limit else start + READ_CAP
    chunk = lines[start:end]
    truncated = len(lines) - (start + len(chunk))
    out = "\n".join(f"{i + 1}\t{line}" for i, line in enumerate(chunk, start))
    if truncated > 0:
        out += f"\n... ({truncated} more lines)"
    return out or "(empty file)"


def tool_write_file(path, content):
    p = Path(path).expanduser()
    if p.parent != p:
        p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(content)
    return f"wrote {len(content)} bytes to {p}"


def tool_edit_file(path, old_string, new_string):
    p = Path(path).expanduser()
    text = p.read_text(errors="replace")
    count = text.count(old_string)
    if count == 0:
        return "error: old_string not found in file"
    if count > 1:
        return f"error: old_string matches {count} times; make it unique"
    p.write_text(text.replace(old_string, new_string, 1))
    return f"edited {p}"


def tool_run_command(command):
    if not YOLO:
        print(f"\n\033[33m[confirm] run: {command}\033[0m")
        if input("allow? [y/N] ").strip().lower() not in ("y", "yes"):
            return "error: user denied this command"
    try:
        proc = subprocess.run(
            command, shell=True, capture_output=True, text=True, timeout=CMD_TIMEOUT
        )
    except subprocess.TimeoutExpired:
        return f"error: command timed out after {CMD_TIMEOUT}s"
    out = proc.stdout + proc.stderr
    if len(out) > OUT_CAP:
        out = out[:OUT_CAP] + f"\n... (truncated, {len(out)} chars total)"
    return f"exit {proc.returncode}\n{out}" if proc.returncode else (out or "(no output)")


TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "read_file",
            "description": "Read a text file. Returns numbered lines.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "offset": {"type": "integer", "description": "1-based start line"},
                    "limit": {"type": "integer", "description": "max lines (default 2000)"},
                },
                "required": ["path"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "write_file",
            "description": "Write (create or overwrite) a file with the given content.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"},
                },
                "required": ["path", "content"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "edit_file",
            "description": "Replace a unique exact string in a file with a new string.",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string", "description": "must match exactly once"},
                    "new_string": {"type": "string"},
                },
                "required": ["path", "old_string", "new_string"],
            },
        },
    },
    {
        "type": "function",
        "function": {
            "name": "run_command",
            "description": "Run a shell command and return stdout+stderr and exit code.",
            "parameters": {
                "type": "object",
                "properties": {"command": {"type": "string"}},
                "required": ["command"],
            },
        },
    },
]

DISPATCH = {
    "read_file": tool_read_file,
    "write_file": tool_write_file,
    "edit_file": tool_edit_file,
    "run_command": tool_run_command,
}


def dispatch(name, arguments):
    note(f"[tool] {name} {short_args(arguments)}")
    fn = DISPATCH.get(name)
    if not fn:
        return f"error: unknown tool {name}"
    try:
        return fn(**arguments)
    except TypeError as e:
        return f"error: bad arguments: {e}"
    except Exception as e:
        return f"error: {e}"


# ---------------------------------------------------------------- api

class Spinner:
    def __enter__(self):
        self._tty = sys.stderr.isatty()
        if self._tty:
            self._stop = threading.Event()
            self._t = threading.Thread(target=self._spin, daemon=True)
            self._t.start()
        return self

    def _spin(self):
        glyphs = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
        i = 0
        while not self._stop.wait(0.1):
            sys.stderr.write(f"\r\033[2m{glyphs[i % len(glyphs)]} waiting for model\033[0m")
            sys.stderr.flush()
            i += 1

    def __exit__(self, *_):
        if self._tty:
            self._stop.set()
            self._t.join()
            sys.stderr.write("\r\033[K")
            sys.stderr.flush()


def chat(messages, token):
    payload = json.dumps({"model": MODEL, "messages": messages, "tools": TOOLS}).encode()
    req = urllib.request.Request(
        f"{BASE_URL}/chat/completions",
        data=payload,
        method="POST",
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {token}",
            "User-Agent": "kimi-harness/0.1",
        },
    )
    with Spinner():
        try:
            with urllib.request.urlopen(req, timeout=600) as resp:
                return json.loads(resp.read())
        except urllib.error.HTTPError as e:
            body = e.read().decode(errors="replace")[:500]
            if e.code == 401:
                sys.exit("401 unauthorized — token expired or invalid. Run `kimi` to refresh "
                         "the CLI login, or set KIMI_API_KEY.")
            sys.exit(f"API error {e.code}: {body}")
        except urllib.error.URLError as e:
            sys.exit(f"network error: {e.reason}")


# ---------------------------------------------------------------- loop

def run_loop(messages, token):
    """Tool-call rounds until the model answers with plain text. Returns that text."""
    for _ in range(MAX_TURNS):
        resp = chat(messages, token)
        usage = resp.get("usage") or {}
        if usage:
            note(f"[tokens] prompt={usage.get('prompt_tokens')} completion={usage.get('completion_tokens')}")
        msg = resp["choices"][0]["message"]
        messages.append(msg)
        if msg.get("reasoning_content"):
            note(f"[thinking] {msg['reasoning_content'][:400]}")
        if msg.get("tool_calls"):
            for tc in msg["tool_calls"]:
                try:
                    args = json.loads(tc["function"].get("arguments") or "{}")
                except json.JSONDecodeError:
                    args = {}
                result = dispatch(tc["function"]["name"], args)
                messages.append({"role": "tool", "tool_call_id": tc["id"], "content": str(result)})
            continue
        return msg.get("content") or ""
    return "(stopped: hit MAX_TURNS without a final answer)"


# ---------------------------------------------------------------- repl

def system_prompt():
    return (
        f"You are a coding agent running in a terminal. Working directory: {os.getcwd()}. "
        f"OS: macOS. Date: {time.strftime('%Y-%m-%d')}. "
        "Use the tools to read, write and edit files and to run shell commands. "
        "Read a file before editing it. Keep changes minimal and verify them by running "
        "code or tests when possible. Answer concisely."
    )


def note(text):
    print(f"\033[2m{text}\033[0m", file=sys.stderr)


def short_args(args):
    parts = []
    for k, v in args.items():
        v = str(v).replace("\n", "\\n")
        parts.append(f"{k}={v[:60]}{'…' if len(v) > 60 else ''}")
    return " ".join(parts)


def main():
    global MODEL, YOLO
    ap = argparse.ArgumentParser(description="minimal Kimi coding harness")
    ap.add_argument("-p", "--prompt", help="one-shot prompt (no REPL)")
    ap.add_argument("--no-yolo", action="store_true",
                    help="confirm each run_command (yolo mode is the default)")
    ap.add_argument("--model", help=f"model id (default {MODEL})")
    args = ap.parse_args()
    if args.model:
        MODEL = args.model
    YOLO = not args.no_yolo

    token = load_token()
    messages = [{"role": "system", "content": system_prompt()}]
    mode = "confirm" if args.no_yolo else "yolo"
    note(f"harness 0.1 — model={MODEL} mode={mode} cwd={os.getcwd()} (exit or Ctrl-D to quit; /reset, /yolo)")

    if args.prompt:
        messages.append({"role": "user", "content": args.prompt})
        print(run_loop(messages, token))
        return

    while True:
        try:
            user = input("\nyou> ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            break
        if not user:
            continue
        if user in ("exit", "quit"):
            break
        if user == "/reset":
            messages[:] = messages[:1]
            note("[history cleared]")
            continue
        if user == "/yolo":
            YOLO = not YOLO
            note(f"[yolo mode {'on — commands auto-approved' if YOLO else 'off — commands need confirmation'}]")
            continue
        messages.append({"role": "user", "content": user})
        try:
            print(f"\nkimi> {run_loop(messages, token)}")
        except KeyboardInterrupt:
            note("[interrupted]")


if __name__ == "__main__":
    main()
