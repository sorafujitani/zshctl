#!/usr/bin/env python3
"""Exercise snippet and fallback completion through a real ZLE/fzf PTY."""

from __future__ import annotations

import fcntl
import os
import pty
import select
import shlex
import shutil
import signal
import struct
import sys
import tempfile
import termios
import time
from contextlib import suppress
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROMPT = b"__ZSHCTL_PROMPT__ "
F12 = b"\x1b[24~"


class Terminal:
    def __init__(self, fd: int) -> None:
        self.fd = fd
        self.output = bytearray()

    def read_once(self, timeout: float) -> bytes:
        ready, _, _ = select.select([self.fd], [], [], timeout)
        if not ready:
            return b""
        try:
            chunk = os.read(self.fd, 65536)
        except OSError:
            return b""
        if b"\x1b[6n" in chunk:
            chunk = chunk.replace(b"\x1b[6n", b"")
            os.write(self.fd, b"\x1b[1;1R")
        self.output.extend(chunk)
        return chunk

    def read_until(self, marker: bytes, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        while marker not in self.output:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError(f"did not receive marker {marker!r}")
            self.read_once(min(remaining, 0.1))

    def wait_for_file(
        self, path: Path, expected: str, timeout: float, description: str
    ) -> str:
        deadline = time.monotonic() + timeout
        while True:
            with suppress(OSError):
                value = path.read_text(encoding="utf-8")
                lines = value.splitlines()
                if expected == "started":
                    if any(line.startswith("done:") for line in lines):
                        raise RuntimeError(
                            "fzf exited before the PTY test could send input"
                        )
                    if "started" in lines:
                        return value
                elif any(line.startswith(expected) for line in lines):
                    return value
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError(f"did not receive {description}")
            self.read_once(min(remaining, 0.1))

    def wait_for_snapshot(self, path: Path, timeout: float) -> tuple[str, int]:
        """Wait for the atomically-written ZLE snapshot file."""
        deadline = time.monotonic() + timeout
        while True:
            with suppress(OSError, UnicodeDecodeError, ValueError):
                line = path.read_text(encoding="utf-8")
                if not line.endswith("\n"):
                    raise ValueError("snapshot is still being written")
                buffer, cursor_text = line.rstrip("\r\n").rsplit("|", 1)
                return buffer, int(cursor_text)

            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError("did not receive a complete ZLE snapshot")
            self.read_once(min(remaining, 0.1))

    def wait_for_prompt(self, timeout: float = 5) -> None:
        self.read_until(PROMPT, timeout)

    def send(self, data: bytes) -> None:
        os.write(self.fd, data)


def atomic_write_script(path: Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")
    path.chmod(0o700)


def main() -> int:
    binary = ROOT / "target" / "release" / "zshctl"
    if not binary.is_file():
        print(f"skip: release binary is missing: {binary}")
        return 0
    if shutil.which("zsh") is None or shutil.which("fzf") is None:
        print("skip: zsh and fzf are required")
        return 0

    default_fzf_tab = (
        Path.home()
        / ".local/share/sheldon/repos/github.com/Aloxaf/fzf-tab/fzf-tab.plugin.zsh"
    )
    fzf_tab_plugin = Path(os.environ.get("ZSHCTL_FZF_TAB_PLUGIN", str(default_fzf_tab)))
    have_fzf_tab = fzf_tab_plugin.is_file()
    autosuggest_plugin = os.environ.get("ZSHCTL_AUTOSUGGEST_PLUGIN")

    with tempfile.TemporaryDirectory(prefix="zshctl-pty-") as directory:
        temporary = Path(directory)
        runtime = temporary / "runtime"
        runtime.mkdir()
        runtime.chmod(0o700)
        config = temporary / "config.yml"
        marker = temporary / "executed.marker"
        ready_file = temporary / "ready"
        snapshot_file = temporary / "snapshot"
        clear_file = temporary / "clear"
        fzf_event = temporary / "fzf-event"
        zcompdump = temporary / "zcompdump"
        completion_root = temporary / "completion-root"
        (completion_root / "pty-completion-target-one").mkdir(parents=True)
        (completion_root / "pty-completion-target-two").mkdir()
        (completion_root / "pty-completion-other").mkdir()

        config.write_text(
            """snippets:
  - name: aws_name_match
    keyword: name_only
    snippet: 'print -r -- name-selected >> "$PTY_MARKER"'
  - name: keyword_only
    keyword: aws_keyword_match
    snippet: 'print -r -- keyword-selected >> "$PTY_MARKER"'
  - name: body_only
    keyword: body_only
    snippet: 'print -r -- aws_body_match >> "$PTY_MARKER"'
  - name: cx_keyword
    keyword: cx_aws_dev
    snippet: 'print -r -- cx-keyword-selected >> "$PTY_MARKER"'
  - name: unrelated_body
    keyword: unrelated_keyword
    snippet: 'print -r -- cx-body-match >> "$PTY_MARKER"'
  - name: same:name
    keyword: colon_keyword
    snippet: 'print -r -- colon-selected >> "$PTY_MARKER"'
""",
            encoding="utf-8",
        )

        real_fzf = shutil.which("fzf")
        assert real_fzf is not None
        snippet_fzf_wrapper = temporary / "snippet-fzf-wrapper"
        fallback_fzf_wrapper = temporary / "fallback-fzf-wrapper"
        fzf_wrapper_prefix = """#!/bin/sh
set +e
event=${PTY_FZF_EVENT_FILE:?PTY_FZF_EVENT_FILE is required}
ready_binding='--bind=start:execute-silent(printf "started\\n" >> "$PTY_FZF_EVENT_FILE")'
"""
        fzf_wrapper_suffix = """status=$?
printf 'done:%s\\n' "$status" >> "$event"
exit "$status"
"""
        atomic_write_script(
            snippet_fzf_wrapper,
            fzf_wrapper_prefix
            + '"$PTY_REAL_FZF" "$@" "$ready_binding"\n'
            + fzf_wrapper_suffix,
        )
        atomic_write_script(
            fallback_fzf_wrapper,
            fzf_wrapper_prefix
            + '"$PTY_REAL_FZF" "$@" "$ready_binding"\n'
            + fzf_wrapper_suffix,
        )

        environment = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("ZSHCTL_")
        }
        environment.update(
            {
                "HOME": os.environ.get("HOME", str(temporary)),
                "PATH": f"{binary.parent}:{os.environ.get('PATH', '')}",
                "TERM": "xterm-256color",
                "ZSHCTL_BIN": str(binary),
                "ZSHCTL_CONFIG": str(config),
                "ZSHCTL_DISABLE_DAEMON": "1",
                "ZSHCTL_RUNTIME_DIR": str(runtime),
                "ZSHCTL_COMPLETION_FALLBACK": "fzf-tab-complete",
                "ZSHCTL_FZF_COMMAND": str(snippet_fzf_wrapper),
                "PTY_MARKER": str(marker),
                "PTY_READY_FILE": str(ready_file),
                "PTY_SNAPSHOT_FILE": str(snapshot_file),
                "PTY_CLEAR_FILE": str(clear_file),
                "PTY_FZF_EVENT_FILE": str(fzf_event),
                "PTY_FZF_TAB_COMMAND": str(fallback_fzf_wrapper),
                "PTY_REAL_FZF": real_fzf,
                "PTY_ZCOMP_DUMP": str(zcompdump),
                "PTY_COMPLETION_ROOT": str(completion_root),
                "PTY_FZF_TAB_PLUGIN": str(fzf_tab_plugin) if have_fzf_tab else "",
                "PTY_AUTOSUGGEST_PLUGIN": autosuggest_plugin or "",
                "TEST_ROOT": str(ROOT),
            }
        )
        startup = (
            "unsetopt beep; stty -ixon -ixoff discard undef; "
            "if [[ -n ${PTY_FZF_TAB_PLUGIN-} ]]; then "
            'fpath=("${PTY_FZF_TAB_PLUGIN:h}" $fpath); '
            'autoload -Uz compinit; compinit -u -d "$PTY_ZCOMP_DUMP"; '
            'source "$PTY_FZF_TAB_PLUGIN"; '
            "zstyle ':fzf-tab:*' fzf-command \"$PTY_FZF_TAB_COMMAND\"; "
            "fi; "
            "if [[ -n ${PTY_AUTOSUGGEST_PLUGIN-} ]]; then "
            'source "$PTY_AUTOSUGGEST_PLUGIN"; '
            "fi; "
            "bindkey -e; "
            'source "$TEST_ROOT/zshctl.zsh"; '
            "zshctl-bind-default-keys; "
            "zshctl-pty-snapshot() { "
            'local tmp="${PTY_SNAPSHOT_FILE}.tmp.$$"; '
            'print -r -- "${BUFFER}|${CURSOR}" >| "$tmp" && '
            'command mv -f "$tmp" "$PTY_SNAPSHOT_FILE"; '
            'print -r -- "__ZSHCTL_SNAPSHOT__${BUFFER}|${CURSOR}"; '
            "}; "
            "zshctl-pty-clear() { "
            'local tmp="${PTY_CLEAR_FILE}.tmp.$$"; '
            "BUFFER=; CURSOR=0; "
            'print -r -- clear >| "$tmp" && '
            'command mv -f "$tmp" "$PTY_CLEAR_FILE"; '
            "}; "
            "zle -N zshctl-pty-snapshot; "
            "zle -N zshctl-pty-clear; "
            "bindkey '^A' zshctl-pty-snapshot; "
            "bindkey '\\e[24~' zshctl-pty-snapshot; "
            "bindkey '^U' zshctl-pty-clear; "
            "PS1='__ZSHCTL_PROMPT__ '; "
            'print -r -- ready >| "${PTY_READY_FILE}.tmp.$$" && '
            'command mv -f "${PTY_READY_FILE}.tmp.$$" "$PTY_READY_FILE"'
        )

        startup_file = temporary / "startup.zsh"
        startup_file.write_text(startup + "\n", encoding="utf-8")

        pid, fd = pty.fork()
        if pid == 0:
            # Execute zsh directly so the PTY test does not add a shell layer.
            os.execvpe("zsh", ["zsh", "-dfi"], environment)  # noqa: S606

        terminal = Terminal(fd)
        winsize = struct.pack("HHHH", 28, 110, 0, 0)
        fcntl.ioctl(fd, termios.TIOCSWINSZ, winsize)
        os.kill(pid, signal.SIGCONT)
        current_case = "startup"
        try:
            # The readiness file is written only after all widgets and bindings exist.
            terminal.send(f"source {shlex.quote(str(startup_file))}\r".encode())
            terminal.wait_for_file(ready_file, "ready", 10, "ZLE readiness")
            terminal.wait_for_prompt()
            terminal.output.clear()

            def clear_line() -> None:
                clear_file.unlink(missing_ok=True)
                snapshot_file.unlink(missing_ok=True)
                fzf_event.unlink(missing_ok=True)
                terminal.output.clear()
                terminal.send(b"\x15")
                terminal.wait_for_file(clear_file, "clear", 5, "ZLE line clear")
                terminal.output.clear()

            def wait_fzf_started() -> None:
                terminal.wait_for_file(fzf_event, "started", 5, "fzf startup")

            def finish_fzf() -> None:
                terminal.wait_for_file(fzf_event, "done:", 5, "fzf completion")
                terminal.wait_for_prompt()

            def take_snapshot() -> tuple[str, int]:
                snapshot_file.unlink(missing_ok=True)
                terminal.output.clear()
                terminal.send(F12)
                snapshot = terminal.wait_for_snapshot(snapshot_file, 5)
                terminal.output.clear()
                return snapshot

            def select_snippet(query: str) -> tuple[str, int]:
                nonlocal current_case
                current_case = f"snippet selection: {query}"
                clear_line()
                terminal.send(query.encode() + b"\t")
                wait_fzf_started()
                terminal.send(b"\r")
                finish_fzf()
                return take_snapshot()

            def search_without_selection(query: str) -> tuple[str, int]:
                nonlocal current_case
                current_case = f"non-keyword search: {query}"
                clear_line()
                terminal.send(b"\x18\x13")
                wait_fzf_started()
                terminal.send(query.encode() + b"\r")
                finish_fzf()
                return take_snapshot()

            expected = {
                "aws_keyword_match": 'print -r -- keyword-selected >> "$PTY_MARKER" ',
                "cx_aws_dev": 'print -r -- cx-keyword-selected >> "$PTY_MARKER" ',
            }
            for query, expected_buffer in expected.items():
                selected_buffer, selected_cursor = select_snippet(query)
                if selected_buffer != expected_buffer or selected_cursor != len(
                    expected_buffer
                ):
                    raise RuntimeError(
                        f"unexpected {query} selection: "
                        f"{selected_buffer!r}, {selected_cursor}"
                    )
                if marker.exists():
                    raise RuntimeError("snippet was executed instead of inserted")

            for query in ("aws_name_match", "aws_body_match", "cx-body-match"):
                selected_buffer, selected_cursor = search_without_selection(query)
                if selected_buffer or selected_cursor:
                    raise RuntimeError(
                        f"non-keyword query selected a snippet: "
                        f"{query!r} -> {selected_buffer!r}, {selected_cursor}"
                    )
                if marker.exists():
                    raise RuntimeError("non-keyword query selected a snippet")

            for cancel_key in (b"\x1b", b"\x03"):
                current_case = f"snippet cancellation: {cancel_key!r}"
                clear_line()
                original = "aws"
                terminal.send(original.encode() + b"\t")
                wait_fzf_started()
                terminal.send(cancel_key)
                finish_fzf()
                cancelled_buffer, cancelled_cursor = take_snapshot()
                if cancelled_buffer != original or cancelled_cursor != len(original):
                    raise RuntimeError(
                        f"unexpected cancellation snapshot: "
                        f"{cancelled_buffer!r}, {cancelled_cursor}"
                    )
                if marker.exists():
                    raise RuntimeError("cancelled snippet was executed")

            if have_fzf_tab:
                current_case = "fzf-tab cd completion"
                clear_line()
                cd_prefix = f"cd {completion_root}/pty-completion-target-"
                terminal.send(cd_prefix.encode() + b"\t")
                wait_fzf_started()
                terminal.send(b"\r")
                finish_fzf()
                cd_buffer, cd_cursor = take_snapshot()
                expected_cd = f"cd {completion_root}/pty-completion-target-one"
                if cd_buffer not in (
                    expected_cd,
                    expected_cd + "/",
                ) or cd_cursor != len(cd_buffer):
                    raise RuntimeError(
                        f"unexpected fzf-tab cd completion: {cd_buffer!r}, {cd_cursor}"
                    )
            else:
                print(f"skip: fzf-tab plugin is missing: {fzf_tab_plugin}")

            current_case = "Ctrl-X Ctrl-S snippet insertion"
            clear_line()
            terminal.send(b"aws\x18\x13")
            wait_fzf_started()
            terminal.send(b"\x15")
            terminal.send(b"aws_keyword_match\r")
            finish_fzf()
            inserted_buffer, inserted_cursor = take_snapshot()
            expected_insert = 'print -r -- keyword-selected >> "$PTY_MARKER" '
            if inserted_buffer != expected_insert or inserted_cursor != len(
                expected_insert
            ):
                raise RuntimeError(
                    f"unexpected Ctrl-X Ctrl-S snapshot: "
                    f"{inserted_buffer!r}, {inserted_cursor}"
                )
            if marker.exists():
                raise RuntimeError("Ctrl-X Ctrl-S snippet was executed")

            terminal.send(b"\x15exit\r")
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                waited, _ = os.waitpid(pid, os.WNOHANG)
                if waited == pid:
                    print(
                        "pty snippet completion: real fzf selection, "
                        "cancellation, fzf-tab fallback, and insertion passed"
                    )
                    return 0
                terminal.read_once(0.1)
            raise RuntimeError("interactive zsh did not exit")
        except (OSError, RuntimeError) as error:
            screen = bytes(terminal.output[-1000:])
            raise RuntimeError(
                f"{current_case}: {error}; last screen={screen!r}"
            ) from None
        finally:
            with suppress(OSError):
                os.close(fd)
            with suppress(ChildProcessError, ProcessLookupError):
                waited, _ = os.waitpid(pid, os.WNOHANG)
                if waited == 0:
                    os.kill(pid, signal.SIGTERM)
                    os.waitpid(pid, 0)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError) as error:
        print(f"pty snippet completion failed: {error}", file=sys.stderr)
        raise SystemExit(1) from None
