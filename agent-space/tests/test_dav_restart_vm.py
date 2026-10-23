#!/usr/bin/env python3
"""Existing-space recovery on an isolated Docker/KVM Host and original disks.

Run serially: python3 tests/test_dav_restart_vm.py --image <candidate>
Optional --baseline-image creates the original space with an older release.
Never mounts a daily-use volume. Credentials remain in memory; evidence is redacted.
"""
import argparse
import concurrent.futures
import hashlib
import http.client
import json
from pathlib import Path
import re
import secrets
import subprocess
import time
import uuid

PROJECT = Path(__file__).resolve().parents[1]
CLI = "/var/lib/microsandbox/.microsandbox/bin/msb"
PYTHON_IMAGE = "nikolaik/python-nodejs:python3.12-nodejs22-slim"


class Qualification:
    def __init__(self, args):
        self.args = args
        self.name = "agent-space-recovery-qual-" + uuid.uuid4().hex[:10]
        self.volume = self.name + "-data"
        self.admin = secrets.token_urlsafe(48)
        self.tokens = [self.admin]
        self.checks = []
        self.holders = []
        self.output = args.output.resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        self.config = self.output / "config.toml"
        text = (PROJECT / "docker/configs.example/config.toml").read_text()
        text = text.replace("max_sessions = 6", "max_sessions = 1")
        text = text.replace("memory_mib = 3072", "memory_mib = 256")
        text = text.replace("workspace_size_mib = 1024", "workspace_size_mib = 256")
        self.config.write_text(text)

    def docker(self, *args, timeout=180):
        result = subprocess.run(["docker", *args], text=True, capture_output=True, timeout=timeout)
        if result.returncode:
            raise RuntimeError(self.redact(result.stderr[-2000:]))
        return result.stdout

    def redact(self, text):
        for token in self.tokens:
            text = text.replace(token, "[REDACTED_SECRET]")
        return text

    def boot(self, image, initialize=False):
        common = ["-e", "AGENT_SPACE_ADMIN_TOKEN=" + self.admin,
                  "-v", self.volume + ":/var/lib/microsandbox",
                  "-v", str(self.config) + ":/etc/agent-space/config.toml:ro"]
        if initialize:
            self.docker("volume", "create", self.volume)
            self.docker("run", "--rm", *common, image, "init-state")
        self.docker("run", "-d", "--name", self.name, "--stop-timeout", "60",
                    "--device", "/dev/kvm", "-p", "127.0.0.1::8932",
                    "-p", "127.0.0.1::8933", *common, image)
        self.wait_ready()

    def http(self, method, path, token=None, body=None, dav=False, timeout=80, headers=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.dav_port if dav else self.main_port,
                                                timeout=timeout)
        headers = {"Authorization": "Bearer " + (token or self.admin), **(headers or {})}
        if isinstance(body, dict):
            body = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        try:
            connection.request(method, path, body, headers)
            response = connection.getresponse()
            return response.status, dict(response.headers), response.read()
        finally:
            connection.close()

    def wait_ready(self):
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            try:
                ports = json.loads(self.docker("inspect", self.name, "--format", "{{json .NetworkSettings.Ports}}"))
                self.main_port = int(ports["8932/tcp"][0]["HostPort"])
                self.dav_port = int(ports["8933/tcp"][0]["HostPort"])
                if self.http("GET", "/healthz", timeout=2)[0] == 200:
                    return
            except (OSError, http.client.HTTPException, KeyError, IndexError):
                pass
            time.sleep(.2)
        raise AssertionError("qualification Host failed to become ready")

    def dav(self, method="PROPFIND", path="", body=None, timeout=15):
        return self.http(method, "/u/recovery/dav/" + path, self.dav_token, body,
                         dav=True, timeout=timeout, headers={"Depth": "1"})

    def cli(self, *args):
        return self.docker("exec", self.name, CLI, *args)

    def catalog(self):
        # The utility container reads only metadata; it never initializes an SDK
        # or touches VM ownership. Use WAL-aware reads on the original volume.
        script = """import hashlib,json,pathlib,sqlite3,sys
c=sqlite3.connect('file:/var/lib/microsandbox/.microsandbox/db/msb.db?mode=ro',uri=True)
c.row_factory=sqlite3.Row
s=c.execute('select id,name,status,config from sandbox where name=?',(sys.argv[1],)).fetchone()
r=c.execute('select id,pid,status,termination_detail from run where sandbox_id=? order by id desc limit 1',(s['id'],)).fetchone()
p=pathlib.Path('/var/lib/microsandbox/agent-space-state/state.json')
count=c.execute('select count(*) from run where sandbox_id=?',(s['id'],)).fetchone()[0]
print(json.dumps({'sandbox_id':s['id'],'name':s['name'],'status':s['status'],'config_sha256':hashlib.sha256(s['config'].encode()).hexdigest(),'run':dict(r),'run_count':count,'state_sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'history':[dict(row) for row in c.execute('select id,pid from run where sandbox_id=? order by id',(s['id'],))]}))
"""
        return json.loads(self.docker("run", "--rm", "--volumes-from", self.name + ":ro",
                                      "--entrypoint", "python3", PYTHON_IMAGE, "-c", script, self.sandbox))

    def check(self, name):
        self.checks.append(name)
        print("PASS", name, flush=True)

    def assert_preserved(self, original):
        snapshot = self.catalog()
        for field in ("sandbox_id", "name", "config_sha256", "state_sha256"):
            assert snapshot[field] == original[field], "original identity/config/credentials changed: " + field
        old_run = next(row for row in snapshot["history"] if row["id"] == original["run"]["id"])
        assert old_run["pid"] == original["run"]["pid"], "recovery must retain the original historical PID"
        assert self.dav("GET", "original.txt")[2] == b"original workspace data\n"
        root = self.cli("exec", self.sandbox, "--", "cat", "/etc/recovery-marker")
        assert "original root data" in root
        return snapshot

    def active(self):
        code, _, body = self.http("GET", "/admin/v1/status")
        assert code == 200
        return json.loads(body)["activeSpaces"]

    def hold(self, shell, *args):
        holder = subprocess.Popen(["docker", "exec", self.name, "sh", "-c", shell, "fixture", *args],
                                  text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.holders.append(holder)
        line = holder.stdout.readline().strip()
        assert line.startswith("held"), "fixture failed to acquire its exact owner: " + line
        return holder

    def hold_lifecycle(self, seconds):
        digest = hashlib.sha256(self.sandbox.encode()).hexdigest()[:32]
        path = "/var/lib/microsandbox/.microsandbox/run/locks/" + digest + ".lock"
        return self.hold('exec 9>"$1"; flock -x 9; echo held; sleep "$2"', path, str(seconds))

    def run(self):
        baseline = self.args.baseline_image or self.args.image
        self.boot(baseline, initialize=True)
        code, _, body = self.http("POST", "/admin/v1/users", body={"username": "recovery", "image": PYTHON_IMAGE})
        assert code == 201
        user = json.loads(body)
        self.tokens.append(user["token"])
        self.mcp_token = user["token"]
        self.space_id = user["agentSpaceId"]
        self.sandbox = "smcp-" + self.space_id
        code, _, body = self.http("POST", "/admin/v1/users/recovery/dav-token", body={"agentSpaceId": self.space_id})
        assert code == 200
        self.dav_token = json.loads(body)["token"]
        self.tokens.append(self.dav_token)
        # Put the original VM PID above the new container's startup PID range.
        self.docker("exec", self.name, "sh", "-c", 'i=0; while [ "$i" -lt 250 ]; do /bin/true; i=$((i+1)); done')
        assert self.dav("PUT", "original.txt", b"original workspace data\n", timeout=80)[0] == 201
        self.cli("exec", self.sandbox, "--", "sh", "-c", "echo 'original root data' > /etc/recovery-marker; sync")
        assert "original root data" in self.cli("exec", self.sandbox, "--", "cat", "/etc/recovery-marker")
        original = self.catalog()
        old_pid = original["run"]["pid"]
        # Restart immediately: identity recovery must not require an old run
        # to age before recognizing a different live executable at its PID.
        self.docker("kill", self.name)
        self.docker("rm", "-v", self.name)
        self.boot(self.args.image)
        crashed = self.catalog()
        assert crashed["status"] == "Crashed", "restart must exercise a real crashed sandbox"
        assert crashed["run"]["pid"] == old_pid
        self.check("abrupt_container_exit_leaves_original_sandbox_crashed")

        shell = 'target="$1"; while :; do sleep 180 & p=$!; if [ "$p" -eq "$target" ]; then echo "held $p"; wait "$p"; exit; fi; kill "$p"; wait "$p" 2>/dev/null; if [ "$p" -gt "$target" ]; then echo "missed $p"; exit 1; fi; done'
        recycled = self.hold(shell, str(old_pid))
        started = time.monotonic()
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            responses = list(pool.map(lambda _: self.dav(), range(4)))
        self.first_dav_seconds = time.monotonic() - started
        assert all(code == 207 and b"original.txt" in body for code, _, body in responses)
        assert self.active() == 1
        assert self.docker("exec", self.name, "cat", "/proc/" + str(old_pid) + "/comm").strip() == "sleep"
        assert recycled.poll() is None, "recovery must not terminate the unrelated recycled PID"
        snapshot = self.assert_preserved(original)
        assert snapshot["run_count"] == original["run_count"] + 1, "concurrent requests must start one VM generation"
        self.check("first_concurrent_dav_recovers_crashed_vm_with_live_recycled_pid")
        self.docker("exec", self.name, "kill", str(old_pid))
        recycled.communicate(timeout=10)

        self.docker("restart", "--time", "60", self.name)
        self.wait_ready()
        assert self.dav()[0] == 207
        self.assert_preserved(original)
        self.check("graceful_container_restart_preserves_original_space_and_files")

        self.cli("pause", self.sandbox)
        assert self.dav()[0] == 207
        self.assert_preserved(original)
        self.check("dav_resumes_existing_paused_vm")

        self.cli("pause", self.sandbox)
        self.docker("kill", self.name)
        self.docker("rm", "-v", self.name)
        self.boot(self.args.image)
        assert self.dav(timeout=15)[0] == 207
        self.assert_preserved(original)
        assert self.active() == 1
        self.check("abrupt_container_exit_recovers_original_paused_vm")

        pid = self.catalog()["run"]["pid"]
        self.docker("exec", self.name, "kill", "-9", str(pid))
        # Signal delivery is not process exit: observe the kernel state without
        # asking MSB to reconcile metadata or replacing the cached Host handle.
        self.docker("exec", self.name, "sh", "-c",
                    'i=0; while [ "$i" -lt 100 ]; do '
                    '[ -r "/proc/$1/stat" ] || exit 0; '
                    'read -r p comm state rest < "/proc/$1/stat" || exit 0; '
                    'case "$state" in Z|X) exit 0;; esac; '
                    'i=$((i+1)); sleep .05; done; exit 1', "sh", str(pid))
        assert self.dav()[0] == 207
        self.assert_preserved(original)
        self.check("cached_runtime_recovers_after_unexpected_vm_process_exit")

        self.cli("stop", self.sandbox)
        self.docker("exec", self.name, "mv", "/usr/local/bin/agent-space-webdav", "/usr/local/bin/agent-space-webdav.qualification-held")
        try:
            assert self.dav()[0] == 503
            assert self.active() == 1, "helper failure keeps its actually running VM's capacity"
        finally:
            self.docker("exec", self.name, "mv", "/usr/local/bin/agent-space-webdav.qualification-held", "/usr/local/bin/agent-space-webdav")
        assert self.dav()[0] == 207
        self.assert_preserved(original)
        self.check("failed_dav_helper_start_is_retryable_without_recreating_vm")

        # Short HTTP budget, bounded backend startup, and a held residency lock.
        self.config.write_text(self.config.read_text().replace("startup_timeout_secs = 60", "startup_timeout_secs = 3"))
        self.docker("restart", "--time", "60", self.name)
        self.wait_ready()
        holder = self.hold_lifecycle(7)
        started = time.monotonic()
        assert self.dav()[0] == 503
        assert time.monotonic() - started < 4.5
        deadline = time.monotonic() + 10
        while self.active() != 0:
            assert time.monotonic() < deadline, "failed stopped VM leaked active capacity"
            time.sleep(.1)
        holder.communicate(timeout=10)
        assert self.dav()[0] == 207
        self.assert_preserved(original)
        self.check("startup_deadline_and_backend_failure_release_terminal_capacity_then_retry")

        self.cli("stop", self.sandbox)
        holder = self.hold_lifecycle(1)
        connection = http.client.HTTPConnection("127.0.0.1", self.dav_port, timeout=1)
        connection.request("PROPFIND", "/u/recovery/dav/", headers={"Authorization": "Bearer " + self.dav_token, "Depth": "1"})
        time.sleep(.2)
        connection.close()
        holder.communicate(timeout=5)
        time.sleep(.5)
        assert self.dav()[0] == 207
        assert self.active() == 1
        self.assert_preserved(original)
        self.check("cancelled_first_dav_waiter_can_retry_and_retains_one_runtime_owner")

        pid = self.catalog()["run"]["pid"]
        self.docker("exec", self.name, "kill", "-STOP", str(pid))
        try:
            assert self.dav()[0] == 503
            assert self.active() == 1, "a stalled resident VM still consumes capacity"
        finally:
            self.docker("exec", self.name, "kill", "-CONT", str(pid))
        assert self.dav()[0] == 207
        self.assert_preserved(original)
        self.check("guest_stall_deadline_is_retryable_and_retains_resident_capacity")

        code, _, body = self.http("POST", "/u/recovery/mcp", self.mcp_token,
                                  {"jsonrpc": "2.0", "id": 1, "method": "initialize",
                                   "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                                              "clientInfo": {"name": "restart-qualification", "version": "1"}}},
                                  headers={"Accept": "application/json, text/event-stream"})
        assert code == 200, "original MCP credential must remain valid"
        raw = body.decode()
        payload = json.loads(next((line[5:].strip() for line in raw.splitlines() if line.startswith("data:") and '"id"' in line), raw))
        assert payload.get("id") == 1 and "result" in payload
        self.check("original_mcp_and_dav_credentials_remain_valid")

        logs = re.sub(r"\x1b\[[0-9;]*m", "", self.docker("logs", self.name))
        lines = [line for line in logs.splitlines() if "DAV startup deadline exceeded" in line or "runtime startup failed" in line or "DAV owned startup failed" in line]
        assert lines and any("vm_start" in line for line in lines)
        assert any("runtime startup failed" in line and re.search(r'\bphase="?vm_start(?:"|\s|$)', line) for line in lines), "failure must retain the original startup phase before reconciliation"
        for line in lines:
            assert all(field in line for field in ("username", "agentSpaceId", "sandboxName", "elapsed_ms", self.space_id))
        self.check("failure_logs_include_stage_identity_and_elapsed_time")
        (self.output / "runtime.log").write_text(self.redact(logs))
        (self.output / "result.json").write_text(json.dumps({
            "image": self.args.image, "image_id": self.docker("image", "inspect", self.args.image, "--format", "{{.Id}}").strip(),
            "baseline_image": baseline, "passed": self.checks,
            "first_dav_seconds": self.first_dav_seconds, "agentSpaceId": self.space_id,
            "sandboxName": self.sandbox, "original_identity": original,
            "final_identity": self.catalog(), "max_sessions": 1,
        }, indent=2) + "\n")

    def cleanup(self):
        subprocess.run(["docker", "rm", "-f", "-v", self.name], capture_output=True)
        for holder in self.holders:
            try:
                holder.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                holder.kill()
                holder.communicate(timeout=5)
        subprocess.run(["docker", "volume", "rm", self.volume], capture_output=True)
        containers = self.docker("ps", "-a", "--format", "{{.Names}}").splitlines()
        volumes = self.docker("volume", "ls", "--format", "{{.Name}}").splitlines()
        clean = self.name not in containers and self.volume not in volumes
        result_path = self.output / "result.json"
        if result_path.exists():
            result = json.loads(result_path.read_text())
            result["cleanup_passed"] = clean
            result_path.write_text(json.dumps(result, indent=2) + "\n")
        assert clean, "DAV restart qualification resources remain"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--baseline-image")
    parser.add_argument("--output", type=Path, default=PROJECT / "target/dav-restart-qualification")
    args = parser.parse_args()
    qualification = Qualification(args)
    try:
        qualification.run()
    except Exception as error:
        try:
            (qualification.output / "failure.log").write_text(qualification.redact(qualification.docker("logs", qualification.name)))
        except Exception:
            pass
        (qualification.output / "failure.json").write_text(json.dumps({"passed": qualification.checks, "error": qualification.redact(str(error))}, indent=2) + "\n")
        raise
    finally:
        qualification.cleanup()


if __name__ == "__main__":
    main()
