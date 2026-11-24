"""Exercise the native Linux client on a graphical session with isolated keyrings."""
import http.client
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[3]
CLIENT = ROOT / "apps/desktop/src-tauri/target/debug/gamesync-desktop"
SERVER = ROOT / "target/debug/gamesync-server"


def require_tools():
    for executable in ("dbus-run-session", "gnome-keyring-daemon"):
        if shutil.which(executable) is None:
            raise RuntimeError(f"{executable} is required")
    for binary in (CLIENT, SERVER):
        if not binary.is_file():
            raise RuntimeError(f"Missing build: {binary}")


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def wait_for(check, description, process):
    deadline = time.monotonic() + 35
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"{description}: process exited with {process.returncode}")
        result = check()
        if result:
            return result
        time.sleep(0.1)
    raise RuntimeError(f"{description}: timed out")


def ready(port):
    connection = http.client.HTTPConnection("127.0.0.1", port, timeout=1)
    try:
        connection.request("GET", "/healthz")
        response = connection.getresponse()
        return response.status == 204
    except ConnectionRefusedError:
        return False
    finally:
        connection.close()


def finish(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def client(base, name, mode, port, code=None):
    instance = base / name
    saves = instance / "saves"
    saves.mkdir(parents=True, exist_ok=True)
    result_file = instance / f"{mode}.result"
    runtime = instance / "run"
    runtime.mkdir(mode=0o700, exist_ok=True)
    input_data = {
        "title": "Verification GameSync",
        "directory": str(saves),
        "executable": str(base / f"{name}-smoke-game"),
        "mode": mode,
        "address": f"http://127.0.0.1:{port}",
        "device": name,
        "code": code,
    }
    environment = {
        **os.environ,
        "XDG_DATA_HOME": str(instance / "data"),
        "XDG_RUNTIME_DIR": str(runtime),
        "NO_AT_BRIDGE": "1",
        "GTK_USE_PORTAL": "0",
        "XDG_CONFIG_HOME": str(instance / "config"),
        "XDG_CACHE_HOME": str(instance / "cache"),
        "GAMESYNC_UI_SMOKE_INPUT": json.dumps(input_data),
        "GAMESYNC_UI_SMOKE_RESULT": str(result_file),
    }
    wayland = environment.get("WAYLAND_DISPLAY")
    parent_runtime = os.environ.get("XDG_RUNTIME_DIR")
    if wayland and parent_runtime and not Path(wayland).is_absolute():
        environment["WAYLAND_DISPLAY"] = str(Path(parent_runtime) / wayland)
    for variable in ("DBUS_SESSION_BUS_ADDRESS", "GNOME_KEYRING_CONTROL"):
        environment.pop(variable, None)
    log_path = instance / f"{mode}.log"
    with log_path.open("wb") as log:
        process = subprocess.Popen([
            "dbus-run-session", "--", "sh", "-c",
            "printf '%s' gamesync-smoke | gnome-keyring-daemon --unlock --components=secrets >/dev/null && exec \"$1\" --background",
            "gamesync-smoke", str(CLIENT),
        ], env=environment, start_new_session=True, stdout=log, stderr=log)
        try:
            wait_for(lambda: result_file.is_file() and result_file.stat().st_size > 0, f"{name} {mode}", process)
            error = json.loads(result_file.read_text())
            if error is not None:
                raise RuntimeError(f"{name} {mode}: {error}")
        except Exception:
            print("\n".join(log_path.read_text(errors="replace").splitlines()[-25:]))
            raise
        finally:
            # End the isolated D-Bus session and its desktop process together.
            if process.poll() is None:
                os.killpg(process.pid, 15)
            finish(process)


def main():
    require_tools()
    with tempfile.TemporaryDirectory(prefix="gamesync-ui-") as workspace:
        base = Path(workspace)
        for machine in ("first", "second"):
            shutil.copy2(shutil.which("sleep"), base / f"{machine}-smoke-game")
        port = free_port()
        env = {**os.environ, "GAMESYNC_DATA": str(base / "server"), "GAMESYNC_PORT": str(port), "GAMESYNC_BIND": "127.0.0.1"}
        server = subprocess.Popen([SERVER], env=env)
        try:
            wait_for(lambda: ready(port), "server startup", server)
            def invite(*flags):
                return subprocess.check_output([SERVER, "invite", *flags], env=env, text=True).strip()

            first = base / "first/saves/progress.sav"
            second = base / "second/saves/progress.sav"
            first.parent.mkdir(parents=True)
            first.write_bytes(b"initial")
            client(base, "first", "seed", port, invite("--admin"))
            client(base, "second", "attach", port, invite())
            assert second.read_bytes() == b"initial"

            second.write_bytes(b"updated on second")
            client(base, "second", "sync", port)
            client(base, "first", "sync", port)
            assert first.read_bytes() == b"updated on second"

            first.write_bytes(b"published while second game is running")
            client(base, "first", "publish-active", port)
            game = subprocess.Popen([base / "second-smoke-game", "90"])
            try:
                client(base, "second", "active", port)
                assert game.poll() is None
                assert second.read_bytes() == b"updated on second"
            finally:
                finish(game)
            client(base, "second", "sync-after-exit", port)
            assert second.read_bytes() == first.read_bytes()

            first.write_bytes(b"independent first")
            second.write_bytes(b"independent second")
            finish(server)
            client(base, "second", "offline", port)
            assert second.read_bytes() == b"independent second"
            server = subprocess.Popen([SERVER], env=env)
            wait_for(lambda: ready(port), "server restart", server)
            client(base, "first", "publish", port)
            client(base, "second", "conflict", port)
            client(base, "first", "resolved", port)
            client(base, "first", "locale", port)
            assert first.read_bytes() == second.read_bytes() == b"independent second"
            print("Native UI smoke passed: pairing, retention, language persistence, multi-device attachment, round trip, protection while playing and conflict resolution.")
        finally:
            finish(server)


if __name__ == "__main__":
    main()
