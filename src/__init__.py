"""Compatibility package for running Python analytics from the repo root.

This extends the ``src`` package path so imports like ``src.dashboard.app``
resolve to ``python-analytics/src`` when uvicorn is started from the top-level
repository directory.
"""

from __future__ import annotations

from pkgutil import extend_path
from pathlib import Path

__path__ = extend_path(__path__, __name__)  # type: ignore[name-defined]

_repo_root = Path(__file__).resolve().parent.parent
_analytics_src = _repo_root / "python-analytics" / "src"
if _analytics_src.is_dir():
    analytics_path = str(_analytics_src)
    if analytics_path not in __path__:
        __path__.append(analytics_path)