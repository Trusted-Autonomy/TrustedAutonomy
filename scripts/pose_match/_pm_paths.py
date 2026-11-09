# (C) Copyright 2026 CinePipeAi, Inc.
"""Path defaults shared by the pose_match research scripts (install / data / project contract).

Import from a script whose sys.path already holds scripts/ (as every pose_match script does):

    sys.dont_write_bytecode = True
    from pose_match import _pm_paths as pmp

- skel_read(rel)   rig asset / derived weights to READ: DATA/train/skel/<rel>, falling back
                   (read-only) to the older <install>/logs/skel/<rel> when only that exists.
- skel_out(rel)    same location for WRITING (never the old install location).
- scratch(name)    intermediate / scratch outputs: DATA/train/pose_match/<name>.
- project_sub(*p)  <project>/<p...> as str, or None when no project folder is configured;
                   pair with need(value, flag) to exit with a plain message.
"""
from __future__ import annotations

import sys
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # scripts/
import _paths  # noqa: E402

INSTALL = _paths.INSTALL


def skel_read(rel: str) -> str:
    new = _paths.skel_dir() / rel
    old = INSTALL / "logs" / "skel" / rel
    return str(old if (not new.exists() and old.exists()) else new)


def skel_out(rel: str) -> str:
    return str(_paths.skel_dir() / rel)


def scratch(name: str) -> str:
    return str(_paths.train_dir() / "pose_match" / name)


def project_sub(*parts: str):
    proj = _paths.project_dir()
    return str(proj.joinpath(*parts)) if proj else None


def need(value, flag: str):
    """Return *value*, or exit with a plain-text message when the default needed a project."""
    if value is None:
        sys.exit(
            f"Error: no project folder, so the default for {flag} is unavailable.\n"
            f"  Pass {flag} DIR or set {_paths.ENV_PROJECT} to the project folder."
        )
    return value
