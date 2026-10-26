"""Checking whether added plugins have a newer version out, for those whose plugin.json "repo" is on GitHub and that
publish their builds as GitHub Releases. Qt-free; the network is injectable.

It only asks GitHub's API for each repo's latest release, at most once a day per repo, and keeps the answers (with
their ETag, so asking again costs nothing against the rate limit when nothing changed) in plugin-updates.json next to
the settings. Being offline or rate limited is normal: it waits (an hour, doubling up to a day, or until GitHub says
the limit resets) and never raises. Downloading a release's .zip is only for the auto-update switch.
"""

import hashlib
import http.client
import json
import logging
import os
import re
import tempfile
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

from telescope import version, web
from telescope.config import config_path

logger = logging.getLogger(__name__)

CHECK_INTERVAL_S = 24 * 60 * 60
WAIT_FIRST_S = 60 * 60  # after a failure; doubles with each one in a row
WAIT_MAX_S = 24 * 60 * 60
REQUEST_TIMEOUT = 15
MAX_REPLY_BYTES = 512 * 1024
MAX_ZIP_BYTES = 20 * 1024 * 1024  # what plugin_loader takes unpacked, so a .zip is never bigger
API = "https://api.github.com/repos/{repo}/releases/latest"
_GITHUB = re.compile(r"https://github\.com/([A-Za-z0-9](?:[A-Za-z0-9-]{0,38}))/([A-Za-z0-9._-]{1,100}?)(?:\.git)?/?\Z")
_VERSION = re.compile(r"[vV]?(\d+(?:\.\d+)*)(.*)\Z")
_MAX_ASSETS = 30


@dataclass(frozen=True)
class Asset:
    name: str
    url: str
    size: int
    sha256: str = ""  # GitHub's digest of it, "" for releases from before it gave one


@dataclass(frozen=True)
class Release:
    tag: str
    page: str  # its page on GitHub
    assets: tuple = ()  # the .zip files it has

    def zip_for(self, name: str) -> Optional[Asset]:
        """The .zip of plugin name: <name>.zip, else one starting with its name, else the only one."""
        exact = [a for a in self.assets if a.name.lower() == f"{name}.zip"]
        starting = [a for a in self.assets if a.name.lower().startswith(name)]
        for found in (exact, starting, list(self.assets)):
            if len(found) == 1:
                return found[0]
        return None


def cache_path() -> Path:
    return config_path().parent / "plugin-updates.json"


def github_repo(url: str) -> Optional[str]:
    """"owner/repo" for a https://github.com/owner/repo address, else None."""
    m = _GITHUB.match(url or "")
    return f"{m.group(1)}/{m.group(2)}" if m else None


def parse_version(text: str) -> Optional[tuple]:
    """(the numbers, whether it has more after them like -beta) for "1.2", "v1.2.0-beta" and the like, else None."""
    m = _VERSION.match((text or "").strip())
    if not m:
        return None
    numbers = tuple(int(n) for n in m.group(1).split("."))
    while len(numbers) > 1 and numbers[-1] == 0:
        numbers = numbers[:-1]
    return numbers, bool(m.group(2).strip(" .-+"))


def is_newer(tag: str, installed: str) -> bool:
    """Whether a release tagged tag is newer than the installed version; False when either can't be read."""
    new, old = parse_version(tag), parse_version(installed)
    if new is None or old is None:
        return False
    if new[0] != old[0]:
        return new[0] > old[0]
    return old[1] and not new[1]  # 1.2 after 1.2-beta


def parse_release(raw: bytes) -> Optional[Release]:
    """The release from GitHub's API reply, None when it isn't one this can use."""
    try:
        data = json.loads(raw.decode("utf-8"))
        tag = data["tag_name"]
        page = data["html_url"]
        if not isinstance(tag, str) or not tag or len(tag) > 100 or not isinstance(page, str):
            return None
        if not page.startswith("https://github.com/"):
            return None
        assets = []
        for a in (data.get("assets") or [])[:_MAX_ASSETS]:
            name, url, size = a.get("name"), a.get("browser_download_url"), a.get("size")
            if not (isinstance(name, str) and name.lower().endswith(".zip") and isinstance(url, str)
                    and url.startswith("https://github.com/") and isinstance(size, int) and 0 < size <= MAX_ZIP_BYTES):
                continue
            digest = a.get("digest") if isinstance(a.get("digest"), str) else ""
            sha = digest[7:].lower() if digest.startswith("sha256:") and len(digest) == 71 else ""
            assets.append(Asset(name, url, size, sha))
        return Release(tag, page, tuple(assets))
    except (ValueError, KeyError, TypeError, AttributeError):
        return None


def _release_to_json(release: Optional[Release]):
    if release is None:
        return None
    return {"tag": release.tag, "page": release.page,
            "assets": [{"name": a.name, "url": a.url, "size": a.size, "sha256": a.sha256} for a in release.assets]}


def _release_from_json(data) -> Optional[Release]:
    try:
        assets = tuple(Asset(str(a["name"]), str(a["url"]), int(a["size"]), str(a.get("sha256", "")))
                       for a in data.get("assets", []))
        return Release(str(data["tag"]), str(data["page"]), assets)
    except (KeyError, TypeError, ValueError, AttributeError):
        return None


class Cache:
    """What the last checks found, by repo, and how long to leave GitHub alone. Reads and writes plugin-updates.json;
    a file that's missing or broken is the same as no checks yet."""

    def __init__(self, path: Optional[Path] = None):
        self.path = path or cache_path()
        self.repos: dict = {}  # "owner/repo": {"checked": time, "etag": str, "release": dict or None}
        self.wait_until = 0.0
        self.failures = 0
        self.auto_installed: dict = {}  # plugin name: the tag auto-update installed last, never installed twice
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return
        if not isinstance(data, dict):
            return
        repos = data.get("repos")
        self.repos = {k: v for k, v in repos.items() if isinstance(k, str) and isinstance(v, dict)} \
            if isinstance(repos, dict) else {}
        self.wait_until = _number(data.get("wait_until"))
        self.failures = int(_number(data.get("failures")))
        auto = data.get("auto_installed")
        self.auto_installed = {k: v for k, v in auto.items() if isinstance(k, str) and isinstance(v, str)} \
            if isinstance(auto, dict) else {}

    def release(self, repo: str) -> Optional[Release]:
        entry = self.repos.get(repo)
        return _release_from_json(entry.get("release")) if entry and entry.get("release") else None

    def due(self, repo: str, now: float) -> bool:
        entry = self.repos.get(repo)
        checked = _number(entry.get("checked")) if entry else 0.0
        return not (0 <= now - checked < CHECK_INTERVAL_S)  # a clock set back counts as due too

    def save(self):
        """Write it; a failure is logged and the next check just asks again."""
        data = {"repos": self.repos, "wait_until": self.wait_until, "failures": self.failures,
                "auto_installed": self.auto_installed}
        try:
            self.path.parent.mkdir(parents=True, exist_ok=True)
            fd, tmp = tempfile.mkstemp(prefix=".plugin-updates-", dir=self.path.parent)
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                json.dump(data, f)
            os.replace(tmp, self.path)
        except OSError:
            logger.warning("Couldn't save the plugin update checks", exc_info=True)


def _number(value) -> float:
    return float(value) if isinstance(value, (int, float)) and not isinstance(value, bool) else 0.0


def _open(url: str, timeout: float, headers: Optional[dict] = None):
    req = urllib.request.Request(url, headers={"User-Agent": f"Telescope/{version.VERSION}", **(headers or {})})
    return web.urlopen(req, timeout)


def _wait_after_limit(exc: urllib.error.HTTPError, now: float) -> float:
    """How long GitHub wants left alone after a 403 or 429: its Retry-After or rate limit reset, at least an hour."""
    headers = exc.headers or {}
    wait = WAIT_FIRST_S
    try:
        wait = max(wait, float(headers.get("Retry-After") or 0))
    except ValueError:
        pass
    try:
        reset = float(headers.get("X-RateLimit-Reset") or 0)
        wait = max(wait, reset - now)
    except ValueError:
        pass
    return min(wait, WAIT_MAX_S)


def _rate_limited(exc: urllib.error.HTTPError) -> bool:
    """A 429, or a 403 that comes with GitHub's rate limit headers (a 403 alone is about the one repo)."""
    headers = exc.headers or {}
    if exc.code == 429:
        return True
    return exc.code == 403 and (headers.get("X-RateLimit-Remaining") == "0" or bool(headers.get("Retry-After")))


def check(repos, cache: Optional[Cache] = None, now: Optional[float] = None, opener: Callable = _open) -> Cache:
    """Ask GitHub about each repo that's due, unless it's being left alone after a failure. Never raises; whatever
    went wrong only makes it wait longer. Returns the cache, saved."""
    cache = cache or Cache()
    now = time.time() if now is None else now
    if now < cache.wait_until and cache.wait_until - now <= WAIT_MAX_S:
        return cache
    asked = False
    for repo in sorted(set(repos)):
        if not cache.due(repo, now):
            continue
        asked = True
        entry = cache.repos.get(repo, {})
        headers = {"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28"}
        if entry.get("etag"):
            headers["If-None-Match"] = entry["etag"]
        try:
            with opener(API.format(repo=repo), REQUEST_TIMEOUT, headers) as r:
                raw = r.read(MAX_REPLY_BYTES + 1)
                etag = r.headers.get("ETag", "") if r.headers else ""
        except urllib.error.HTTPError as exc:
            if exc.code == 304:  # nothing new since the ETag, and it didn't count against the limit
                cache.repos[repo] = dict(entry, checked=now)
                continue
            if _rate_limited(exc):
                cache.wait_until = now + _wait_after_limit(exc, now)
                logger.info("GitHub is limiting plugin update checks; trying again in %d min",
                            (cache.wait_until - now) // 60)
                break
            if 400 <= exc.code < 500:  # no release, or this repo is blocked or gone: ask again tomorrow, others now
                if exc.code != 404:
                    logger.info("GitHub said HTTP %d about %s", exc.code, repo)
                cache.repos[repo] = {"checked": now, "etag": "", "release": None}
                continue
            _failed(cache, now, f"HTTP {exc.code}")
            break
        except (OSError, ValueError, http.client.HTTPException) as exc:
            _failed(cache, now, type(exc).__name__)
            break
        release = parse_release(raw) if len(raw) <= MAX_REPLY_BYTES else None
        if release is None:
            logger.info("The latest release of %s couldn't be read", repo)
        cache.repos[repo] = {"checked": now, "etag": etag if release else "", "release": _release_to_json(release)}
    else:
        if asked:
            cache.wait_until, cache.failures = 0.0, 0
    if asked:
        cache.save()
    return cache


def _failed(cache: Cache, now: float, why: str):
    cache.failures += 1
    wait = min(WAIT_FIRST_S * 2 ** min(cache.failures - 1, 10), WAIT_MAX_S)
    cache.wait_until = now + wait
    logger.info("Couldn't check for plugin updates (%s); trying again in %d min", why, wait // 60)


class DownloadError(Exception):
    pass


def download(asset: Asset, dest_dir: Path, opener: Callable = _open) -> Path:
    """Download a release's .zip into dest_dir, checked against its size and (when GitHub gave one) its sha256.
    Raises DownloadError, in English since it only goes to the log."""
    if asset.size > MAX_ZIP_BYTES or not asset.url.startswith("https://"):
        raise DownloadError(f"{asset.name} isn't a plugin .zip this downloads")
    target = dest_dir / "plugin.zip"
    digest, done = hashlib.sha256(), 0
    try:
        with opener(asset.url, REQUEST_TIMEOUT, {"Accept": "application/octet-stream"}) as r, target.open("wb") as f:
            while chunk := r.read(256 * 1024):
                done += len(chunk)
                if done > asset.size:
                    raise DownloadError(f"{asset.name} is bigger than GitHub said")
                digest.update(chunk)
                f.write(chunk)
    except (OSError, http.client.HTTPException) as exc:
        raise DownloadError(f"{asset.name} couldn't be downloaded ({type(exc).__name__})") from exc
    if done != asset.size:
        raise DownloadError(f"{asset.name} was cut short")
    if asset.sha256 and digest.hexdigest() != asset.sha256:
        raise DownloadError(f"{asset.name} didn't match its checksum")
    return target
