import json
import math
import os
import subprocess
import sys
import time
from pathlib import Path

from benchmark import FILESYSTEMS, METRICS, metric_key


STATUS = "FFF performance"
MARKER = "<!-- fff-performance -->"
ISSUE = "FFF benchmark reference drift"
LIMIT = 10
SUITE = "FFF search latency"
CONFIG = json.loads(Path(__file__).with_name("config.json").read_text())


class Run:
    def __init__(self, directory):
        self.directory = directory
        self.repo = os.environ["GITHUB_REPOSITORY"]
        self.repo_api = f"repos/{self.repo}"
        self.url = f"https://github.com/{self.repo}/actions/runs/{os.environ['GITHUB_RUN_ID']}"
        event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
        self.pr = event.get("pull_request") if os.environ["GITHUB_EVENT_NAME"] == "pull_request_target" else None

    def api(self, endpoint, data=None, *, token=None, method="POST", pages=False):
        args = ["api", endpoint, "-H", "X-GitHub-Api-Version: 2026-03-10"]
        if data is not None:
            args += ["--method", method, "--input", "-"]
        if pages:
            args += ["--paginate", "--slurp"]
        return json.loads(gh(args, token, data))

    def current_pr(self):
        pr = self.api(f"{self.repo_api}/pulls/{self.pr['number']}")
        return pr["state"] == "open" and pr["head"]["sha"] == self.pr["head"]["sha"]

    def status(self, state, description):
        self.api(f"{self.repo_api}/statuses/{self.pr['head']['sha']}", {
            "context": STATUS, "state": state, "description": description, "target_url": self.url,
        })

    def dispatch(self):
        if self.pr and not self.current_pr():
            with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
                output.write("stale=true\n")
            return
        inputs = {"kind": "reference"}
        if self.pr:
            self.status("pending", "Queued for measurement on load")
            inputs = {
                "kind": "pr", "base_sha": self.pr["base"]["sha"], "head_sha": self.pr["head"]["sha"],
            }
        runner_repo = os.environ["FFF_PERF_RUNNER_REPO"]
        token = os.environ["FFF_PERF_RUNNER_TOKEN"]
        if not token:
            raise ValueError("Set the FFF_PERF_RUNNER_TOKEN Actions secret")
        endpoint = f"repos/{runner_repo}/actions"
        response = self.api(f"{endpoint}/workflows/benchmark.yml/dispatches", {
            "ref": "main", "inputs": inputs,
        }, token=token)
        run_id = str(int(response["workflow_run_id"]))
        print(f"Queued measurement run {run_id}", flush=True)
        deadline = time.monotonic() + 75 * 60
        while time.monotonic() < deadline:
            run = self.api(f"{endpoint}/runs/{run_id}", token=token)
            if run["status"] == "completed":
                if run["conclusion"] != "success":
                    raise RuntimeError(f"Measurement run {run_id} finished with {run['conclusion']}")
                gh([
                    "run", "download", run_id, "--repo", runner_repo,
                    "--name", "fff-measurements", "--dir", str(self.directory),
                ], token)
                return
            time.sleep(15)
        raise TimeoutError(f"Measurement run {run_id} exceeded the wait time")

    def prepare(self):
        rows = read_rows(self.directory, "compare" if self.pr else "reference")
        if not self.pr:
            return
        benches = {
            side: [{"name": metric_key(row), "unit": "ms", "value": row[f"{side}_ms"]}
                   for row in rows if gated(row)]
            for side in ("base", "head")
        }
        repo_url = f"https://github.com/{self.repo}"
        base = self.pr["base"]["sha"]
        date = int(time.time() * 1000)
        baseline = {
            "lastUpdate": date,
            "repoUrl": repo_url,
            "entries": {SUITE: [{
                "commit": {"id": base, "url": f"{repo_url}/commit/{base}"},
                "date": date,
                "tool": "customSmallerIsBetter",
                "benches": benches["base"],
            }]},
        }
        (self.directory / "baseline.json").write_text(json.dumps(baseline))
        (self.directory / "head.json").write_text(json.dumps(benches["head"]))

    def publish(self):
        prepared = os.environ["FFF_PREPARE_OUTCOME"] == "success"
        rows = read_rows(self.directory, "compare" if self.pr else "reference") if prepared else []
        if self.pr:
            regressed = any(gated(row) and ratio(row) > 1 + LIMIT / 100 for row in rows)
            if os.environ["FFF_GATE_OUTCOME"] == "success":
                state, message = "success", "Search latency is within the regression threshold"
            elif regressed:
                state, message = "failure", f"Search latency regressed by more than {LIMIT}%"
            else:
                state, message = "error", "Performance check could not complete"
            self.status(state, message)
            body = self.report(message, rows)
            if self.current_pr():
                self.comment(body)
            self.summary(body)
        else:
            drift = any(gated(row) and (ratio(row) > 1 + LIMIT / 100 or ratio(row) < 1 - LIMIT / 100)
                        for row in rows)
            message = "Daily reference is within the drift threshold"
            if drift:
                message = f"Daily reference drifted by more than {LIMIT}%"
            elif not prepared:
                message = "Daily reference measurement could not complete"
            body = self.report(message, rows)
            self.summary(body)
            if drift or not prepared:
                self.alert(body)
                raise RuntimeError(message)

    def report(self, message, rows):
        detail = ""
        if self.pr:
            detail = f"\n\nBase `{self.pr['base']['sha'][:12]}` · PR `{self.pr['head']['sha'][:12]}`"
        else:
            detail = f"\n\nPinned release `{CONFIG['reference']['release']}`"
        table = ""
        if rows:
            table = (
                "\n\n| Filesystem | Repository | Metric | Base | Measured | Change |\n"
                "| --- | --- | --- | ---: | ---: | ---: |\n"
            )
            for row in rows:
                change = 100 * (ratio(row) - 1)
                metric = "first_file (informational)" if row["metric"] == "first_file" else row["metric"]
                table += (f"| {row['filesystem']} | {row['fixture']} | {metric} | "
                          f"{row['base_ms']:.3f} ms | {row['head_ms']:.3f} ms | {change:+.1f}% |\n")
        return f"**{message}**{detail}{table}\n[Workflow run]({self.url})"

    def comment(self, body):
        endpoint = f"{self.repo_api}/issues/{self.pr['number']}/comments"
        pages = self.api(f"{endpoint}?per_page=100", pages=True)
        body = f"{MARKER}\n{body}"
        for page in pages:
            for comment in page:
                if comment["user"]["login"] == "github-actions[bot]" and comment["body"].startswith(MARKER):
                    self.api(f"{self.repo_api}/issues/comments/{comment['id']}", {"body": body}, method="PATCH")
                    return
        self.api(endpoint, {"body": body})

    def alert(self, body):
        endpoint = f"{self.repo_api}/issues"
        pages = self.api(f"{endpoint}?state=open&creator=github-actions%5Bbot%5D&per_page=100", pages=True)
        for page in pages:
            for issue in page:
                if issue["title"] == ISSUE and issue["body"].startswith(MARKER):
                    self.api(f"{endpoint}/{issue['number']}/comments", {"body": body})
                    return
        self.api(endpoint, {
            "title": ISSUE, "body": f"{MARKER}\n{body}", "assignees": [self.repo.split("/")[0]],
        })

    def summary(self, body):
        with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as output:
            output.write(body + "\n")


def gh(args, token=None, data=None):
    env = os.environ.copy()
    if token is not None:
        env["GH_TOKEN"] = token
    return subprocess.check_output(
        ["gh", *args], env=env, text=True, timeout=90,
        input=json.dumps(data) if data is not None else "",
    )


def read_rows(directory, mode):
    artifact = directory / "fff-measurements.json"
    if artifact.stat().st_size > 32768:
        raise ValueError("Measurement artifact is too large")
    result = json.loads(artifact.read_text())
    if result["version"] != 1 or result["mode"] != mode:
        raise ValueError("Unexpected measurement artifact type")
    expected = [(filesystem, name, metric) for filesystem in FILESYSTEMS
                for name in CONFIG["fixtures"] for metric in METRICS]
    rows = result["rows"]
    if [(row["filesystem"], row["fixture"], row["metric"]) for row in rows] != expected:
        raise ValueError("Measurement artifact has unexpected metric names")
    for row in rows:
        for field in ("base_ms", "head_ms"):
            value = row[field]
            if type(value) not in (float, int) or not math.isfinite(value) or not 0 < value <= 20000:
                raise ValueError("Measurement artifact has invalid timing")
    return rows


def gated(row):
    return row["metric"] != "first_file"


def ratio(row):
    return row["head_ms"] / row["base_ms"]


if __name__ == "__main__":
    mode, directory = sys.argv[1:]
    run = Run(Path(directory))
    {"dispatch": run.dispatch, "prepare": run.prepare, "publish": run.publish}[mode]()
