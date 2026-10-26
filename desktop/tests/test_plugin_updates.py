"""The update check for added plugins: versions, GitHub's answers, and staying quiet offline or rate limited."""

import hashlib
import io
import json
import urllib.error
from email.message import Message

import pytest

from telescope import plugin_updates
from telescope.plugin_updates import Asset, Cache, Release

REPO = "someone/their_plugin"
NOW = 1_800_000_000.0


@pytest.mark.parametrize("tag, installed, newer", [
    ("v1.3", "1.2", True),
    ("1.10", "1.9", True),
    ("v2", "1.9.9", True),
    ("1.2", "1.2", False),
    ("v1.2.0", "1.2", False),
    ("1.1", "1.2", False),
    ("1.2", "1.2-beta", True),
    ("1.2-beta", "1.2", False),
    ("1.3-beta", "1.2", True),
    ("latest", "1.2", False),
    ("1.3", "", False),
    ("1.3", "first", False),
])
def test_versions_compare_by_their_numbers(tag, installed, newer):
    assert plugin_updates.is_newer(tag, installed) is newer


@pytest.mark.parametrize("url, repo", [
    ("https://github.com/someone/their_plugin", REPO),
    ("https://github.com/someone/their_plugin/", REPO),
    ("https://github.com/someone/their_plugin.git", REPO),
    ("https://github.com/someone/their_plugin/tree/main", None),
    ("https://gitlab.com/someone/their_plugin", None),
    ("https://github.com.evil.org/someone/their_plugin", None),
    ("", None),
])
def test_only_github_repos_are_followed(url, repo):
    assert plugin_updates.github_repo(url) == repo


def _headers(**values) -> Message:
    msg = Message()
    for key, value in values.items():
        msg[key.replace("_", "-")] = str(value)
    return msg


class _Reply(io.BytesIO):
    def __init__(self, data: bytes, **headers):
        super().__init__(data)
        self.headers = _headers(**headers)


def _release_json(tag="v1.3", assets=None) -> bytes:
    return json.dumps({"tag_name": tag, "html_url": f"https://github.com/{REPO}/releases/tag/{tag}",
                       "assets": assets if assets is not None else [
                           {"name": "their_plugin.zip", "size": 1000, "digest": "sha256:" + "a" * 64,
                            "browser_download_url": f"https://github.com/{REPO}/releases/download/{tag}/their_plugin.zip"},
                           {"name": "notes.txt", "size": 10,
                            "browser_download_url": f"https://github.com/{REPO}/releases/download/{tag}/notes.txt"}]
                       }).encode()


class _GitHub:
    """Stands in for the network: answers each request with the next thing in replies, and records what was asked."""

    def __init__(self, *replies):
        self.replies = list(replies)
        self.asked = []

    def __call__(self, url, timeout, headers=None):
        self.asked.append((url, dict(headers or {})))
        reply = self.replies.pop(0)
        if isinstance(reply, BaseException):
            raise reply
        return reply


def _http_error(code, **headers):
    return urllib.error.HTTPError("https://api.github.com/x", code, "nope", _headers(**headers), io.BytesIO(b""))


@pytest.fixture
def cache(tmp_path):
    return Cache(tmp_path / "plugin-updates.json")


def test_a_release_is_found_kept_and_not_asked_for_again_the_same_day(cache):
    github = _GitHub(_Reply(_release_json(), ETag='"abc"'))
    plugin_updates.check([REPO], cache, now=NOW, opener=github)
    assert github.asked[0][0] == f"https://api.github.com/repos/{REPO}/releases/latest"
    release = Cache(cache.path).release(REPO)  # saved to the file
    assert release.tag == "v1.3" and [a.name for a in release.assets] == ["their_plugin.zip"]
    assert release.zip_for("their_plugin").sha256 == "a" * 64
    plugin_updates.check([REPO], Cache(cache.path), now=NOW + 3600, opener=github)
    assert len(github.asked) == 1


def test_asking_again_sends_the_etag_and_a_304_keeps_what_was_found(cache):
    github = _GitHub(_Reply(_release_json(), ETag='"abc"'), _http_error(304))
    plugin_updates.check([REPO], cache, now=NOW, opener=github)
    cache = plugin_updates.check([REPO], Cache(cache.path), now=NOW + plugin_updates.CHECK_INTERVAL_S, opener=github)
    assert github.asked[1][1]["If-None-Match"] == '"abc"'
    assert cache.release(REPO).tag == "v1.3" and not cache.due(REPO, NOW + plugin_updates.CHECK_INTERVAL_S + 60)


def test_a_repo_without_releases_is_nothing_to_say(cache):
    cache = plugin_updates.check([REPO], cache, now=NOW, opener=_GitHub(_http_error(404)))
    assert cache.release(REPO) is None and cache.wait_until == 0 and not cache.due(REPO, NOW + 60)


def test_offline_it_waits_longer_each_time_and_never_raises(cache):
    github = _GitHub(*[urllib.error.URLError("no network")] * 3, _Reply(_release_json()))
    cache = plugin_updates.check([REPO], cache, now=NOW, opener=github)
    assert cache.wait_until == NOW + 3600 and cache.release(REPO) is None
    assert plugin_updates.check([REPO], cache, now=NOW + 60, opener=github) is cache and len(github.asked) == 1
    cache = plugin_updates.check([REPO], cache, now=NOW + 3600, opener=github)
    assert cache.wait_until == NOW + 3600 + 7200
    cache = plugin_updates.check([REPO], Cache(cache.path), now=NOW + 3600 + 7200, opener=github)  # kept in the file
    assert cache.failures == 3 and cache.wait_until == NOW + 3600 + 7200 + 4 * 3600
    cache = plugin_updates.check([REPO], cache, now=cache.wait_until, opener=github)
    assert cache.release(REPO).tag == "v1.3" and cache.failures == 0 and cache.wait_until == 0


def test_the_wait_stops_at_a_day(cache):
    cache.failures = 20
    cache = plugin_updates.check([REPO], cache, now=NOW, opener=_GitHub(TimeoutError()))
    assert cache.wait_until == NOW + plugin_updates.WAIT_MAX_S


def test_rate_limited_it_waits_until_github_says_and_stops_asking(cache):
    github = _GitHub(_http_error(403, X_RateLimit_Remaining=0, X_RateLimit_Reset=int(NOW + 5 * 3600)))
    cache = plugin_updates.check([REPO, "other/repo"], cache, now=NOW, opener=github)
    assert len(github.asked) == 1 and cache.wait_until == NOW + 5 * 3600  # the second repo wasn't asked either
    assert plugin_updates.check([REPO], cache, now=NOW + 4 * 3600, opener=github) is cache


@pytest.mark.parametrize("headers, wait", [
    ({"Retry-After": "120"}, 3600),  # at least an hour
    ({"Retry-After": "7200"}, 7200),
    ({"X-RateLimit-Reset": str(int(NOW + 10 * 24 * 3600))}, 24 * 3600),  # never more than a day
    ({"Retry-After": "soon"}, 3600),
])
def test_a_429_waits_as_long_as_it_says_within_reason(cache, headers, wait):
    error = urllib.error.HTTPError("https://api.github.com/x", 429, "slow down", Message(), io.BytesIO(b""))
    for key, value in headers.items():
        error.headers[key] = value
    cache = plugin_updates.check([REPO], cache, now=NOW, opener=_GitHub(error))
    assert cache.wait_until == NOW + wait


def test_a_clock_set_back_doesnt_leave_it_waiting_for_days(cache):
    cache.wait_until = NOW + 30 * 24 * 3600
    cache.repos[REPO] = {"checked": NOW + 30 * 24 * 3600}
    github = _GitHub(_Reply(_release_json()))
    cache = plugin_updates.check([REPO], cache, now=NOW, opener=github)
    assert len(github.asked) == 1 and cache.release(REPO).tag == "v1.3"


@pytest.mark.parametrize("raw", [
    b"<html>", b"[]", json.dumps({"tag_name": "v1"}).encode(),
    json.dumps({"tag_name": "v1", "html_url": "https://evil.org/x"}).encode(),
])
def test_a_reply_that_isnt_a_release_is_ignored(cache, raw):
    cache = plugin_updates.check([REPO], cache, now=NOW, opener=_GitHub(_Reply(raw)))
    assert cache.release(REPO) is None and cache.wait_until == 0


def test_assets_that_arent_plugin_zips_from_github_are_left_out():
    release = plugin_updates.parse_release(_release_json(assets=[
        {"name": "a.zip", "size": 10, "browser_download_url": "https://evil.org/a.zip"},
        {"name": "b.zip", "size": 10 ** 9, "browser_download_url": "https://github.com/x/y/b.zip"},
        {"name": "c.zip", "size": 10, "browser_download_url": "https://github.com/x/y/c.zip", "digest": "md5:x"},
    ]))
    assert [(a.name, a.sha256) for a in release.assets] == [("c.zip", "")]


def test_a_broken_cache_file_is_no_checks_yet(tmp_path):
    path = tmp_path / "plugin-updates.json"
    path.write_text("{nope")
    assert Cache(path).repos == {}
    path.write_text(json.dumps({"repos": {REPO: {"release": {"tag": 5}}}, "wait_until": "x"}))
    cache = Cache(path)
    assert cache.wait_until == 0 and cache.release(REPO) is None


@pytest.mark.parametrize("names, want", [
    (["their_plugin.zip", "other.zip"], "their_plugin.zip"),
    (["their_plugin-1.3.zip", "other.zip"], "their_plugin-1.3.zip"),
    (["build.zip"], "build.zip"),
    (["a.zip", "b.zip"], None),
])
def test_the_right_zip_is_picked_for_the_plugin(names, want):
    release = Release("v1", "https://github.com/x/y", tuple(Asset(n, "https://github.com/" + n, 1) for n in names))
    found = release.zip_for("their_plugin")
    assert (found.name if found else None) == want


def test_a_download_is_checked_against_its_size_and_digest(tmp_path):
    data = b"PK" + b"x" * 98
    good = Asset("p.zip", "https://github.com/p.zip", len(data), hashlib.sha256(data).hexdigest())
    assert plugin_updates.download(good, tmp_path, opener=lambda *a: _Reply(data)).read_bytes() == data
    with pytest.raises(plugin_updates.DownloadError, match="checksum"):
        plugin_updates.download(Asset("p.zip", good.url, len(data), "0" * 64), tmp_path, opener=lambda *a: _Reply(data))
    with pytest.raises(plugin_updates.DownloadError, match="bigger"):
        plugin_updates.download(Asset("p.zip", good.url, 10), tmp_path, opener=lambda *a: _Reply(data))
    with pytest.raises(plugin_updates.DownloadError, match="cut short"):
        plugin_updates.download(Asset("p.zip", good.url, 1000), tmp_path, opener=lambda *a: _Reply(data))
    with pytest.raises(plugin_updates.DownloadError, match="couldn't be downloaded"):
        plugin_updates.download(good, tmp_path, opener=_GitHub(urllib.error.URLError("offline")))
