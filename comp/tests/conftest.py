# comp/tests/conftest.py
"""Pytest path setup so tests can import sibling scripts directly."""
from __future__ import annotations

import sys
from pathlib import Path

# comp/scripts/ contains free-standing scripts (compare.py, dashboard.py,
# clusters.py). They're invoked as scripts, not imported as a package, so
# we add comp/scripts/ to sys.path and import top-level (e.g. `import
# clusters`). Mirrors how dashboard.py imports clusters at runtime.
COMP_SCRIPTS = Path(__file__).resolve().parent.parent / "scripts"
if str(COMP_SCRIPTS) not in sys.path:
    sys.path.insert(0, str(COMP_SCRIPTS))
