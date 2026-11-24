"""Verify that a packaged Linux client starts and initializes its data."""
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile

from smoke import finish, wait_for


def initialized(database):
    if not database.is_file():
        return False
    with sqlite3.connect(database.as_uri() + "?mode=ro", uri=True) as connection:
        tables = connection.execute("SELECT name FROM sqlite_master WHERE type = 'table'")
        return {"config", "activity"}.issubset(name for (name,) in tables)


def main(binary):
    with tempfile.TemporaryDirectory(prefix="gamesync-package-") as workspace:
        base = Path(workspace)
        data = base / "data"
        log = base / "output.log"
        env = {
            **os.environ,
            "XDG_DATA_HOME": str(data),
            "XDG_CONFIG_HOME": str(base / "config"),
            "XDG_CACHE_HOME": str(base / "cache"),
            "APPIMAGE_EXTRACT_AND_RUN": "1",
            "NO_AT_BRIDGE": "1",
        }
        with log.open("wb") as output:
            process = subprocess.Popen(
                [str(binary), "--background"],
                env=env, stdout=output, stderr=output, start_new_session=True,
            )
            try:
                database = data / "io.gamesync.desktop" / "client.sqlite"
                wait_for(lambda: initialized(database), "package initialization", process)
            except Exception:
                print(log.read_text(errors="replace"), file=sys.stderr)
                raise
            finally:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                finish(process)
        print(f"Initialized GameSync from {binary.name}")


if __name__ == "__main__":
    main(Path(sys.argv[1]).resolve(strict=True))
