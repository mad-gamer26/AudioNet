"""Drives deploy/official/install.sh through a terminal, the way a person
answers it: runs a command under a pseudo-terminal, waits for each
question, types the answer, and returns everything the installer said.

    from install_sh import run
    out = run(["sh", "-c", "cat install.sh | sh"], [("Install folder", ""), ...])

Each step is (text that must appear before the prompt, answer to type);
the step waits for the text and for the line to end in a prompt (the
installer's questions end with a space and no newline). A question that
does not come within the timeout fails the step with the transcript so far.
Used by install_sh_linux.py and install_sh_mac.py; also handy by hand:

    python3 scripts/test/install_sh.py COMMAND...   (answers typed live)
"""
import os, pty, re, select, sys, time


class Failed(Exception):
    pass


def run(cmd, steps, timeout=180, env=None, echo=True):
    """Runs `cmd` under a pty with `steps`; returns (exit status, transcript)."""
    pid, fd = pty.fork()
    if pid == 0:
        if env is not None:
            os.execvpe(cmd[0], cmd, env)
        os.execvp(cmd[0], cmd)
    out = b""
    seen = 0  # transcript position after the last answered question
    steps = list(steps)

    def read_some(wait):
        nonlocal out
        r, _, _ = select.select([fd], [], [], wait)
        if not r:
            return True
        try:
            data = os.read(fd, 4096)
        except OSError:
            return False
        if not data:
            return False
        out += data
        if echo:
            sys.stdout.write(data.decode("utf-8", "replace"))
            sys.stdout.flush()
        return True

    end = time.time() + timeout
    alive = True
    while alive and steps:
        want, answer = steps[0]
        text = out[seen:].decode("utf-8", "replace")
        # A question: the wanted text, then a prompt left open at the end.
        i = text.find(want)
        if i >= 0 and re.search(r"[:?)] $", text[i:]) and not text[i:].endswith("\n"):
            time.sleep(0.2)  # make sure nothing more follows
            if not read_some(0.1) or out[seen:].decode("utf-8", "replace") == text:
                os.write(fd, (answer + "\n").encode())
                seen = len(out)
                steps.pop(0)
                end = time.time() + timeout
                continue
        if time.time() > end:
            os.kill(pid, 9)
            raise Failed(f"no question {want!r} within {timeout} s\n--- transcript ---\n{out.decode('utf-8', 'replace')}")
        alive = read_some(0.5)
    while alive:
        alive = read_some(1)
        if time.time() > end:
            os.kill(pid, 9)
            raise Failed(f"did not finish within {timeout} s\n--- transcript ---\n{out.decode('utf-8', 'replace')}")
    _, status = os.waitpid(pid, 0)
    text = out.decode("utf-8", "replace").replace("\r\n", "\n")
    if steps:
        raise Failed(f"ended before the question {steps[0][0]!r}\n--- transcript ---\n{text}")
    return os.waitstatus_to_exitcode(status), text


if __name__ == "__main__":
    os.execvp(sys.argv[1], sys.argv[1:])
