#!/usr/bin/env python3
"""Build a standalone Windows DOSO-OTA管理台.exe using PyInstaller."""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

CONSOLE_DIR = Path(__file__).resolve().parent
REPO_ROOT = CONSOLE_DIR.parent.parent
SERVER_PY = CONSOLE_DIR / "server.py"
WEB_DIR = CONSOLE_DIR / "web"
OPS_PY = REPO_ROOT / "tools" / "deploy" / "doso-ota-ops.py"
DEFAULT_DIST = REPO_ROOT / "artifacts" / "ota-console"
EXE_NAME = "DOSO-OTA管理台"

EXCLUDES = [
    "numpy",
    "scipy",
    "pandas",
    "matplotlib",
    "PIL",
    "torch",
    "torchvision",
    "tensorflow",
    "cv2",
    "sklearn",
    "IPython",
    "jupyter",
    "notebook",
    "pytest",
    "PyQt5",
    "PyQt6",
    "PySide2",
    "PySide6",
    "wx",
    "lxml",
    "cryptography",
    "OpenSSL",
    "setuptools",
    "pkg_resources",
    "pip",
    "wheel",
    "sqlite3",
    "unittest",
]


def main() -> int:
    dist_dir = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else DEFAULT_DIST
    dist_dir.mkdir(parents=True, exist_ok=True)
    sep = ";" if sys.platform == "win32" else ":"

    with tempfile.TemporaryDirectory(prefix="doso-ota-exe-build-") as tmp:
        tmp_path = Path(tmp)
        cmd = [
            sys.executable,
            "-m",
            "PyInstaller",
            "--noconfirm",
            "--clean",
            "--onefile",
            "--console",
            "--name",
            EXE_NAME,
            "--distpath",
            str(dist_dir),
            "--workpath",
            str(tmp_path / "work"),
            "--specpath",
            str(tmp_path / "spec"),
            "--add-data",
            f"{WEB_DIR}{sep}web",
            "--add-data",
            f"{OPS_PY}{sep}.",
            "--hidden-import",
            "tkinter",
            "--hidden-import",
            "tkinter.filedialog",
        ]
        for mod in EXCLUDES:
            cmd.extend(["--exclude-module", mod])
        cmd.append(str(SERVER_PY))

        print("Running:", " ".join(cmd), flush=True)
        res = subprocess.run(cmd)
        if res.returncode != 0:
            return res.returncode

    suffix = ".exe" if sys.platform == "win32" else ""
    out_exe = dist_dir / f"{EXE_NAME}{suffix}"
    if not out_exe.is_file():
        print(f"error: expected output missing: {out_exe}", file=sys.stderr)
        return 1
    size_mb = out_exe.stat().st_size / (1024 * 1024)
    print(f"BUILD_OK: {out_exe} ({size_mb:.2f} MB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
