"""Sessions on its own: plain stand-ins for sources and plugins, no window and no Qt."""

from types import SimpleNamespace

from telescope.session import Sessions, StreamSession


def _source(sid):
    return SimpleNamespace(id=sid, session=None)


def _stream(sessions, sid, worker=None):
    source = _source(sid)
    sessions.add(source, StreamSession(id=1, url=f"http://{sid}/", client=None, worker=worker, source=source))
    return source


def _plugin(follows_focus=True):
    return SimpleNamespace(follows_focus=follows_focus)


def test_streams_are_kept_in_the_order_they_started_and_forgotten_when_they_stop():
    sessions = Sessions()
    a, b = _stream(sessions, "a"), _stream(sessions, "b")
    sessions.add(a, a.session)  # a new worker for the same stream doesn't add it twice
    assert sessions.streams == [a, b]
    sessions.forget(a)
    assert sessions.streams == [b] and a.session is None
    assert sessions.find("b") is b and sessions.find("a") is None


def test_the_panels_follow_focus_and_the_mic_stays_with_main():
    sessions = Sessions()
    a, b = _stream(sessions, "a"), _stream(sessions, "b")
    sessions.focus, sessions.main = b, a
    follower, mic = _plugin(), _plugin(follows_focus=False)
    plugins = [follower, mic]
    assert sessions.hearing(plugins, b) == [follower]
    assert sessions.hearing(plugins, a) == [mic]
    sessions.focus = a
    assert sessions.hearing(plugins, a) == [follower, mic]
    assert sessions.followers(plugins) == [follower] and sessions.stays(plugins) == [mic]
    assert sessions.background() == [b] and sessions.shown is a.session


def test_a_source_being_woken_counts_as_streaming_from_it():
    sessions = Sessions()
    phone = _source("phone")
    assert not sessions.is_streaming_from("phone")
    sessions.waking, sessions.wake_source = True, phone
    assert sessions.is_streaming_from("phone") and sessions.is_waking("phone") and sessions.is_starting()
    assert not sessions.is_waking("other")


def test_the_active_source_is_the_shown_stream_then_the_wake_then_what_start_would_use():
    sessions = Sessions()
    picked = _source("picked")
    asked = []

    def selected():
        asked.append(True)
        return picked

    assert sessions.active(selected) is picked
    phone = _source("phone")
    sessions.waking, sessions.wake_source = True, phone
    assert sessions.active(selected) is phone
    shown = _stream(sessions, "shown")
    sessions.focus = shown
    assert sessions.active(selected) is shown
    assert asked == [True]  # only asked while nothing else answers


def test_a_worker_finds_its_stream():
    sessions = Sessions()
    worker = object()
    _stream(sessions, "a")
    b = _stream(sessions, "b", worker=worker)
    assert sessions.by_worker(worker) is b and sessions.by_worker(object()) is None


def test_a_stop_drops_what_waited_for_the_start():
    sessions = Sessions()
    sessions.restart_later.add("a")
    sessions.held_states["a"] = ("source", 1, {})
    sessions.start_later.append(lambda: None)
    sessions.drop_queued()
    assert not (sessions.restart_later or sessions.held_states or sessions.start_later)
