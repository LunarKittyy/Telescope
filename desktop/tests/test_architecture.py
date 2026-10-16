"""The rules in ARCHITECTURE.md that a test can check: who may import and call whom."""

import ast
import re
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent / "telescope"
PLUGINS = sorted(p for p in (ROOT / "plugins").glob("*.py") if p.name != "__init__.py")


def _imports(path: Path) -> set:
    tree = ast.parse(path.read_text(encoding="utf-8"))
    found = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            found.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.module:
            found.add(node.module)
            found.update(f"{node.module}.{alias.name}" for alias in node.names)
    return found


@pytest.mark.parametrize("path", PLUGINS, ids=lambda p: p.stem)
def test_a_plugin_imports_no_other_plugin(path):
    others = {f"telescope.plugins.{p.stem}" for p in PLUGINS if p != path}
    assert not _imports(path) & others, "share it through a module outside plugins/, the bus or the host"


@pytest.mark.parametrize("path", PLUGINS, ids=lambda p: p.stem)
def test_a_plugin_imports_neither_the_window_nor_the_sessions(path):
    assert not {"telescope.app", "telescope.session", "telescope.sources"} & _imports(path)


@pytest.mark.parametrize("path", PLUGINS, ids=lambda p: p.stem)
def test_a_plugin_calls_nothing_private_on_its_host(path):
    text = path.read_text(encoding="utf-8")
    assert not re.findall(r"\bhost\._[a-z]\w*", text), "add it to HostServices instead"


@pytest.mark.parametrize("name", ["sources.py", "session.py", "stream_flow.py", "plugin.py"])
def test_the_pieces_the_window_drives_dont_import_it(name):
    assert "telescope.app" not in _imports(ROOT / name)


def test_sessions_needs_no_qt():
    assert not any(m.startswith("PyQt6") for m in _imports(ROOT / "session.py"))
