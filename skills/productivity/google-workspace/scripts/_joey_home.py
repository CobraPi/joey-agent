"""Resolve JOEY_HOME for standalone skill scripts.

Skill scripts may run outside the Joey process (e.g. system Python,
nix env, CI) where ``joey_constants`` is not importable.  This module
provides the same ``get_joey_home()`` and ``display_joey_home()``
contracts as ``joey_constants`` without requiring it on ``sys.path``.

When ``joey_constants`` IS available it is used directly so that any
future enhancements (profile resolution, Docker detection, etc.) are
picked up automatically.  The fallback path replicates the core logic
from ``joey_constants.py`` using only the stdlib.

All scripts under ``google-workspace/scripts/`` should import from here
instead of duplicating the ``JOEY_HOME = Path(os.getenv(...))`` pattern.
"""

from __future__ import annotations

import os
from pathlib import Path

try:
    from joey_constants import display_joey_home as display_joey_home
    from joey_constants import get_joey_home as get_joey_home
except (ModuleNotFoundError, ImportError):

    def get_joey_home() -> Path:
        """Return the Joey home directory (default: ~/.joey).

        Mirrors ``joey_constants.get_joey_home()``."""
        val = os.environ.get("JOEY_HOME", "").strip()
        return Path(val) if val else Path.home() / ".joey"

    def display_joey_home() -> str:
        """Return a user-friendly ``~/``-shortened display string.

        Mirrors ``joey_constants.display_joey_home()``."""
        home = get_joey_home()
        try:
            return "~/" + str(home.relative_to(Path.home()))
        except ValueError:
            return str(home)
