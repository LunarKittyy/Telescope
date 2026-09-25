"""Which Telescope this is. Release builds get telescope/_build.py from CI; source checkouts read the
repo's VERSION file and report channel "dev"."""

from pathlib import Path

try:
    from telescope import _build  # written by scripts/write_build_info.py in CI
except ImportError:
    _build = None


def _source_version() -> str:
    try:
        return (Path(__file__).resolve().parent.parent.parent / "VERSION").read_text().strip()
    except OSError:
        return "0.0.0"


VERSION: str = getattr(_build, "VERSION", None) or _source_version()
BUILD: int = int(getattr(_build, "BUILD", 0) or 0)
CHANNEL: str = getattr(_build, "CHANNEL", None) or "dev"
COMMIT: str = getattr(_build, "COMMIT", None) or ""

REPO = "LunarKittyy/Telescope"


def display_version() -> str:
    """What the UI shows: "0.5.0", "0.5.0 nightly 123" or "0.5.0 dev"."""
    if CHANNEL == "stable":
        return VERSION
    if CHANNEL == "dev":
        return f"{VERSION} dev"
    return f"{VERSION} {CHANNEL} {BUILD}"


def release_asset_url(name: str) -> str:
    """Download link for a file from the release this build belongs to (dev builds use nightly)."""
    tag = f"v{VERSION}" if CHANNEL == "stable" else "nightly"
    return f"https://github.com/{REPO}/releases/download/{tag}/{name}"
