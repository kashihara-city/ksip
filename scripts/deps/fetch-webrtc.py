"""Fetch the pinned official Google WebRTC checkout with pinned depot_tools."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import subprocess
from contextlib import contextmanager

ROOT = Path(__file__).resolve().parents[2]
TEMP = ROOT / "temp"


def run(args: list[str], cwd: Path | None = None, env: dict[str, str] | None = None):
    subprocess.run(args, cwd=cwd, env=env, check=True)


def output(args: list[str], cwd: Path) -> str:
    return subprocess.check_output(args, cwd=cwd, text=True).strip()


@contextmanager
def short_windows_path(path: Path):
    """Keep sources under temp while avoiding legacy tool path limits."""
    if os.name != "nt":
        yield path
        return
    drive = next((f"{letter}:" for letter in "ZYXWVUT"
                  if not Path(f"{letter}:\\").exists()), None)
    if not drive:
        raise RuntimeError("No free drive letter for the WebRTC checkout")
    run(["subst", drive, str(path)])
    try:
        yield Path(drive + "\\")
    finally:
        run(["subst", drive, "/d"])


def main():
    lock = json.loads((ROOT / "deps/native-sources.lock.json").read_text())
    depot = lock["depot-tools"]
    webrtc = lock["google-webrtc"]
    depot_dir = TEMP / "d"
    checkout = TEMP / "w"
    source = checkout / "src"
    TEMP.mkdir(parents=True, exist_ok=True)

    if not (depot_dir / ".git").is_dir():
        run(["git", "clone", depot["url"], str(depot_dir)])
    run(["git", "fetch", "origin", depot["commit"], "--depth=1"], depot_dir)
    run(["git", "checkout", "--detach", depot["commit"]], depot_dir)
    if output(["git", "rev-parse", "HEAD"], depot_dir) != depot["commit"]:
        raise RuntimeError("depot_tools revision mismatch")

    checkout.mkdir(parents=True, exist_ok=True)
    gclient = checkout / ".gclient"
    gclient.write_text(
        "solutions = [{\n"
        "  'name': 'src',\n"
        f"  'url': '{webrtc['url']}',\n"
        "  'deps_file': 'DEPS',\n"
        "  'managed': False,\n"
        "  'custom_deps': {},\n"
        "  'custom_vars': {},\n"
        "}]\n",
        encoding="utf-8",
    )
    env = os.environ.copy()
    env["PATH"] = str(depot_dir) + os.pathsep + env["PATH"]
    env["DEPOT_TOOLS_UPDATE"] = "0"
    env["DEPOT_TOOLS_WIN_TOOLCHAIN"] = "0"
    run([str(depot_dir / "bootstrap/win_tools.bat")], depot_dir, env)
    gclient_cmd = str(depot_dir / "gclient.bat")
    with short_windows_path(checkout) as short_checkout:
        run([gclient_cmd, "sync", "--revision", f"src@{webrtc['commit']}",
             "--no-history", "--force"], short_checkout, env)

    if output(["git", "rev-parse", "HEAD"], source) != webrtc["commit"]:
        raise RuntimeError("WebRTC revision mismatch")
    digest = hashlib.sha256((source / "DEPS").read_bytes()).hexdigest()
    if digest != webrtc["deps_sha256"]:
        raise RuntimeError("WebRTC DEPS hash mismatch")
    build = source / "build"
    if output(["git", "rev-parse", "HEAD"], build) != webrtc["build_commit"]:
        raise RuntimeError("WebRTC build dependency revision mismatch")
    print("Verified Google WebRTC", webrtc["commit"])


if __name__ == "__main__":
    main()
