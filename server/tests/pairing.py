"""Run with python3 server/tests/pairing.py after cargo build -p gamesync-server."""
import http.client
import json
import os
import socket
import subprocess
import tempfile
import time
import uuid
from pathlib import Path

binary = Path("target/debug/gamesync-server")
if os.name == "nt":
    binary = binary.with_suffix(".exe")

with socket.socket() as probe:
    probe.bind(("127.0.0.1", 0))
    port = probe.getsockname()[1]

with tempfile.TemporaryDirectory() as data:
    env = {**os.environ, "GAMESYNC_DATA": data, "GAMESYNC_PORT": str(port)}
    invite = subprocess.check_output([binary, "invite"], env=env, text=True).strip()
    server = subprocess.Popen([binary], env=env)

    def request(method, path, body=None, token=None, extra_headers=None):
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = f"Bearer {token}"
        headers.update(extra_headers or {})
        try:
            connection.request(method, path, json.dumps(body) if isinstance(body, dict) else body, headers)
            response = connection.getresponse()
            return response.status, response.read()
        finally:
            connection.close()

    try:
        for _ in range(30):
            try:
                assert request("GET", "/healthz")[0] == 204
                break
            except ConnectionRefusedError:
                time.sleep(0.1)
        else:
            raise RuntimeError("server did not start")

        game = f"/v1/games/{uuid.uuid4()}"
        assert request("GET", game + "/head")[0] == 401
        status, response = request("POST", "/v1/pair", {"code": invite, "name": "PC Test"})
        assert status == 200
        credentials = json.loads(response)
        assert request("POST", "/v1/pair", {"code": invite, "name": "Replay"})[0] == 401
        token = credentials["token"]
        digest = "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        upload = f"/v1/uploads/{digest}/3"
        download = f"/v1/objects/{digest}/3"
        assert request("PUT", upload + "/0", b"a", token)[0] == 200
        assert request("PUT", upload + "/0", b"a", token)[0] == 409
        assert request("GET", download, token=token)[0] == 404
        server.terminate()
        server.wait()
        server = subprocess.Popen([binary], env=env)
        for _ in range(30):
            try:
                assert json.loads(request("GET", upload, token=token)[1]) == {"offset": 1, "complete": False}
                break
            except ConnectionRefusedError:
                time.sleep(0.1)
        else:
            raise RuntimeError("server did not restart")
        assert request("PUT", upload + "/1", b"bc", token)[0] == 200
        assert request("GET", download, token=token, extra_headers={"Range": "bytes=1-"}) == (206, b"bc")
        publication = {"request": str(uuid.uuid4()), "base": None, "manifest": []}
        assert request("POST", game + "/head", {**publication, "machine": str(uuid.uuid4())}, token)[0] == 422
        status, response = request("POST", game + "/head", publication, token)
        assert status == 200
        revision = json.loads(response)["revision"]
        status, response = request("GET", game + f"/revisions/{revision}", token=token)
        assert status == 200 and json.loads(response)["machine"] == credentials["device"]
        subprocess.run([binary, "revoke", credentials["device"]], env=env, check=True)
        assert request("GET", game + "/head", token=token)[0] == 401
        print("Pairing, publication attribution and revocation verified")
    finally:
        server.terminate()
        server.wait()
