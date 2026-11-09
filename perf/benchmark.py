import argparse
import contextlib
import fcntl
import hashlib
import json
import os
import select
import shutil
import stat
import statistics
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path


RUNTIME = "fff-perf-gvisor"
DOCKER = ["docker", "--host=unix:///var/run/docker.sock"]
METRICS = ("first_file", "find_files", "grep")
FILESYSTEMS = ("ext4", "btrfs")
DEFAULT_STATE = Path.home() / ".local/share/fff-perf"
MAX_REPLY = 1024 * 1024
MAX_BINARY = 64 * 1024 * 1024


class BenchmarkError(RuntimeError):
    pass


def command(*args):
    return subprocess.check_output(args, text=True, timeout=30).strip()


def fixture_paths(config, state, prepare=False):
    paths = {}
    for filesystem in FILESYSTEMS:
        root = state / filesystem
        if prepare and filesystem == "ext4":
            root.mkdir(parents=True, exist_ok=True)
        types = command("findmnt", "-n", "-o", "FSTYPE", "-T", str(root)).splitlines()
        if set(types) != {filesystem}:
            raise BenchmarkError(f"{root} must be on {filesystem}, found {types}")
        for name, fixture in config["fixtures"].items():
            revision = fixture["revision"]
            path = root / f"{name}-{revision[:12]}"
            if prepare and not path.exists():
                with tempfile.TemporaryDirectory(dir=root, prefix=".checkout-") as tmp:
                    checkout = Path(tmp) / "repo"
                    subprocess.run(["git", "init", "-q", checkout], check=True)
                    git = ["git", "-C", str(checkout), "-c", "core.hooksPath=/dev/null"]
                    subprocess.run(
                        [*git, "fetch", "-q", "--depth=1", fixture["url"], revision],
                        check=True, timeout=300,
                    )
                    subprocess.run([*git, "checkout", "-q", "--detach", revision], check=True)
                    checkout.rename(path)
            if command("git", "-C", str(path), "rev-parse", "HEAD") != revision:
                raise BenchmarkError(f"Unexpected Git revision in {path}")
            if command("git", "-C", str(path), "status", "--porcelain", "--untracked-files=all"):
                raise BenchmarkError(f"Fixture checkout has changes in {path}")
            if not (path / fixture["expected_path"]).is_file():
                raise BenchmarkError(f"Expected search result is missing in {path}")
            paths[(filesystem, name)] = path
    return paths


class Sandbox:
    def __init__(self, config, state):
        self.config = config
        self.label = "io.fff.perf.worker=" + hashlib.sha256(os.fsencode(state)).hexdigest()[:16]
        self.image = command(*DOCKER, "image", "inspect", "-f", "{{.Id}}", config["image"])

    def clean(self):
        containers = command(*DOCKER, "ps", "-aq", "--filter", f"label={self.label}")
        for container in containers.splitlines():
            command(*DOCKER, "rm", "-f", container)

    @contextlib.contextmanager
    def run(self, binary, fixture):
        name = f"fff-perf-{uuid.uuid4().hex}"
        args = [
            *DOCKER, "run", "-i", "--name", name, "--label", self.label,
            "--runtime", RUNTIME, "--pull", "never", "--network", "none",
            "--read-only", "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
            "--user", f"{os.getuid()}:{os.getgid()}", "--pids-limit", "256",
            "--memory", "2g", "--memory-swap", "2g", "--log-driver", "none",
            "--cpuset-cpus", self.config["cpus"],
            "--tmpfs", "/tmp:rw,nosuid,nodev,noexec,size=64m,mode=1777",
            "--env", "HOME=/tmp", "--env", f"RAYON_NUM_THREADS={self.config['threads']}",
            "--mount", f"type=bind,src={binary},dst=/fff,readonly,bind-recursive=disabled",
            "--mount", f"type=bind,src={fixture},dst=/fixture,readonly,bind-recursive=disabled",
            "--workdir", "/fixture", "--entrypoint", "/fff", self.image,
            "--no-update-check", "--no-watch", "--log-file", "/tmp/fff.log",
            "--log-level", "error", "/fixture",
        ]
        with subprocess.Popen(
            args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0,
        ) as proc:
            try:
                yield proc
            except (BenchmarkError, BrokenPipeError) as exc:
                if proc.poll() == 125:
                    os.set_blocking(proc.stderr.fileno(), False)
                    message = (proc.stderr.read(2048) or b"").decode(errors="replace")
                    raise BenchmarkError(f"Docker failed to start {RUNTIME}: {message.strip()!r}") from exc
                raise
            finally:
                proc.stdin.close()
                try:
                    proc.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait()
                self.clean()


class Client:
    def __init__(self, proc):
        self.proc = proc
        self.pending = bytearray()
        self.request = 0

    def send(self, method, params=None, request_id=None):
        message = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        if request_id is not None:
            message["id"] = request_id
        try:
            self.proc.stdin.write(json.dumps(message).encode() + b"\n")
        except BrokenPipeError as exc:
            raise BenchmarkError("FFF closed MCP stdin") from exc

    def call(self, method, params):
        self.request += 1
        self.send(method, params, self.request)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            line_end = self.pending.find(b"\n")
            if line_end == -1:
                if not select.select([self.proc.stdout], [], [], max(0, deadline - time.monotonic()))[0]:
                    break
                block = os.read(self.proc.stdout.fileno(), 65536)
                if not block:
                    raise BenchmarkError(f"FFF exited during {method}")
                self.pending.extend(block)
                if len(self.pending) > MAX_REPLY:
                    raise BenchmarkError("MCP response exceeds size limit")
                continue
            try:
                message = json.loads(self.pending[:line_end])
            except (ValueError, RecursionError) as exc:
                raise BenchmarkError("Invalid MCP response") from exc
            del self.pending[:line_end + 1]
            if not isinstance(message, dict):
                raise BenchmarkError("Invalid MCP response object")
            if message.get("id") == self.request:
                result = message.get("result")
                if not isinstance(result, dict) or result.get("isError"):
                    raise BenchmarkError(f"MCP {method} returned an error")
                return result
        raise BenchmarkError(f"MCP {method} timed out")

    def search(self, name, arguments, expected):
        result = self.call("tools/call", {"name": name, "arguments": arguments})
        content = result.get("content")
        if not isinstance(content, list) or not any(
            isinstance(item, dict) and isinstance(item.get("text"), str) and expected in item["text"]
            for item in content
        ):
            raise BenchmarkError(f"FFF {name} did not return the expected result")


def measure(sandbox, binary, path, fixture, repeats):
    with sandbox.run(binary, path) as proc:
        client = Client(proc)
        client.call("initialize", {
            "protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "fff-perf", "version": "1"},
        })
        client.send("notifications/initialized")
        find = ({"query": fixture["file_query"], "maxResults": 20}, fixture["expected_path"])
        grep = ({
            "query": fixture["grep_query"], "mode": "literal",
            "context": 0, "maxResults": 30,
        }, fixture["grep_query"])
        start = time.perf_counter_ns()
        client.search("find_files", *find)
        first = time.perf_counter_ns() - start
        timings = {}
        for tool, args in (("find_files", find), ("grep", grep)):
            client.search(tool, *args)
            samples = []
            for _ in range(repeats):
                start = time.perf_counter_ns()
                client.search(tool, *args)
                samples.append(time.perf_counter_ns() - start)
            timings[tool] = statistics.median(samples)
        return {"first_file": first, **timings}


def compare(config, sandbox, paths, base, head):
    settings = config["worker"]
    rows = []
    for (filesystem, fixture_name), path in paths.items():
        print(f"Measuring {filesystem} / {fixture_name}", flush=True)
        fixture = config["fixtures"][fixture_name]
        variants = [("base", base), ("head", head)]
        if base is None:
            variants = [("head", head)]
        for _, binary in variants:
            measure(sandbox, binary, path, fixture, 1)

        samples = {label: [] for label, _ in variants}
        for pair in range(settings["pairs"]):
            order = reversed(variants) if pair % 2 else variants
            for label, binary in order:
                samples[label].append(measure(sandbox, binary, path, fixture, settings["repeats"]))

        for metric in METRICS:
            rows.append({
                "filesystem": filesystem,
                "fixture": fixture_name,
                "metric": metric,
                **{
                    f"{label}_ms": statistics.median(sample[metric] for sample in results) / 1e6
                    for label, results in samples.items()
                },
            })
    return rows


def stage_binary(source, destination):
    with os.fdopen(os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK), "rb") as binary:
        info = os.fstat(binary.fileno())
        header = binary.read(20)
        if (not stat.S_ISREG(info.st_mode) or info.st_size > MAX_BINARY
                or not header.startswith(b"\x7fELF\x02\x01") or header[18:20] != b"\x3e\x00"):
            raise BenchmarkError("Expected a regular x86-64 ELF binary under 64 MiB")
        binary.seek(0)
        with destination.open("xb") as staged:
            shutil.copyfileobj(binary, staged)
    destination.chmod(0o555)
    return destination


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--state", type=Path, default=DEFAULT_STATE)
    subcommands = parser.add_subparsers(dest="mode", required=True)
    subcommands.add_parser("prepare")
    for mode in ("compare", "calibrate", "reference"):
        action = subcommands.add_parser(mode)
        action.add_argument("--output", type=Path, required=True)
        if mode == "compare":
            action.add_argument("--base", type=Path, required=True)
            action.add_argument("--head", type=Path, required=True)
        if mode == "calibrate":
            action.add_argument("--replace-reference", action="store_true")
    args = parser.parse_args()
    config = json.loads(Path(__file__).with_name("config.json").read_text())
    state = args.state.resolve()
    state.mkdir(mode=0o700, parents=True, exist_ok=True)
    with (state / "benchmark.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        paths = fixture_paths(config, state, prepare=args.mode == "prepare")
        if args.mode == "prepare":
            return
        execute(args, config, state, paths)


def execute(args, config, state, paths):
    reference = config["reference"]
    known_good = state / "reference" / "baseline.json"
    config_digest = hashlib.sha256(json.dumps(config, sort_keys=True).encode()).hexdigest()
    sandbox = Sandbox(config["worker"], state)
    sandbox.clean()
    if args.mode == "calibrate" and known_good.exists() and not args.replace_reference:
        raise BenchmarkError("A reference already exists. Use --replace-reference to update it.")
    if args.mode == "reference":
        saved = json.loads(known_good.read_text())
        if saved["config"] != config_digest:
            raise BenchmarkError("Fixture or benchmark settings changed. Recalibrate the reference.")

    with tempfile.TemporaryDirectory(dir=state, prefix="binaries-") as work:
        work = Path(work)
        if args.mode == "compare":
            base = stage_binary(args.base, work / "base")
            head = stage_binary(args.head, work / "head")
        else:
            head = stage_binary(state / "reference" / reference["binary"], work / "reference")
            with head.open("rb") as binary:
                if hashlib.file_digest(binary, "sha256").hexdigest() != reference["sha256"]:
                    raise BenchmarkError("Pinned reference binary checksum mismatch")
            base = head if args.mode == "calibrate" else None
        rows = compare(config, sandbox, paths, base, head)

    if args.mode == "reference":
        for row in rows:
            row["base_ms"] = saved["timings"][metric_key(row)]
    elif args.mode == "calibrate":
        timings = {metric_key(row): (row["base_ms"] + row["head_ms"]) / 2 for row in rows}
        save_json(known_good, {"config": config_digest, "timings": timings})

    save_json(args.output, {"version": 1, "mode": args.mode, "rows": rows})


def metric_key(row):
    return "/".join(row[field] for field in ("filesystem", "fixture", "metric"))


def save_json(path, value):
    with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as file:
        temporary = Path(file.name)
        try:
            json.dump(value, file, indent=2, allow_nan=False)
            file.write("\n")
            file.close()
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


if __name__ == "__main__":
    try:
        main()
    except (BenchmarkError, OSError, ValueError, subprocess.SubprocessError) as error:
        sys.exit(str(error))
