"""Records what the Python app does for Auto framing and Expose for my face, for framing_tests.rs and face_exposure_tests.rs to
compare the Rust plugins with.

    cd desktop && QT_QPA_PLATFORM=offscreen python ../rust/crates/effects/tests/fixtures/make_framing_fixtures.py

Writes next to this file: framing.json (the box, the window, the smoothing and the spring on made-up and real poses, what settings
load to, and whole runs of the plugin tick by tick) and face_exposure.json (the metering point's moves and whole runs of the plugin).
"""
import json
import random
import sys
from dataclasses import replace
from pathlib import Path

import cv2
from PyQt6.QtWidgets import QApplication

HERE = Path(__file__).resolve().parent
DESKTOP = HERE.parents[4] / "desktop"
sys.path.insert(0, str(DESKTOP))

from telescope import vision  # noqa: E402
from telescope.people import Track  # noqa: E402
from telescope.plugin import EventBus  # noqa: E402
from telescope.plugins import face_exposure, framing  # noqa: E402
from telescope.plugins.face_exposure import FaceExposurePlugin, FaceMeter  # noqa: E402
from telescope.plugins.framing import (  # noqa: E402
    FramingPlugin, Spring, Target, _Latch, _Room, fit, follow, hands_out, inside, smoothed, target_box, tuning,
)
from telescope.view import View, view_to_shown  # noqa: E402
from telescope.vision import Person  # noqa: E402

ROTATIONS = {None: None, 90: cv2.ROTATE_90_CLOCKWISE, 180: cv2.ROTATE_180, 270: cv2.ROTATE_90_COUNTERCLOCKWISE}
ASPECT = 16 / 9


def record(person: Person) -> dict:
    return {name: list(p) for name, p in sorted(person.points.items())}


def facing(x=0.5, y=0.4, width=0.1, **extra) -> Person:
    points = {
        "nose": (x, y - 0.12, 0.99), "left_eye": (x + 0.01, y - 0.14, 0.99), "right_eye": (x - 0.01, y - 0.14, 0.99),
        "left_shoulder": (x + width / 2, y, 0.99), "right_shoulder": (x - width / 2, y, 0.99),
        "right_wrist": (x - width / 2, y + 0.3, 0.99),
        "left_hip": (x + width / 3, y + 0.35, 0.9), "right_hip": (x - width / 3, y + 0.35, 0.9),
    }
    points.update(extra)
    return Person(points)


def poses() -> list:
    """People of every kind target_box and hands_out tell apart, and the astronaut the vision tests' real models found."""
    real = json.loads((HERE.parents[2] / "vision/tests/fixtures/vision.json").read_text())["people"]
    found = [Person({n: tuple(p) for n, p in pose.items()}) for pose in real]
    base = facing()
    feet = Person({**base.points, "left_ankle": (0.53, 0.95, 0.9), "right_ankle": (0.47, 0.95, 0.9)})
    hand = facing(x=0.65, left_wrist=(0.85, 0.45, 0.99), left_elbow=(0.75, 0.47, 0.99))
    hands = Person({**hand.points, "left_index": (0.87, 0.43, 0.2), "left_thumb": (0.86, 0.46, 0.2), "left_pinky": (0.86, 0.44, 0.1)})
    both = Person({**base.points, "right_wrist": (0.3, 0.4, 0.4), "right_elbow": (0.38, 0.42, 0.4), "left_wrist": (0.7, 0.4, 0.3),
                   "left_elbow": (0.62, 0.42, 0.3)})
    guess = facing(left_wrist=(0.69, 0.44, 0.3), left_elbow=(0.62, 0.46, 0.3))
    no_head = Person({n: p for n, p in hand.points.items() if n not in ("nose", "left_eye", "right_eye")})
    no_hips = Person({n: p for n, p in base.points.items() if "hip" not in n})
    shoulders = Person({n: p for n, p in base.points.items() if "shoulder" in n})
    unsure = Person({**base.points, "left_shoulder": (0.55, 0.4, 0.3)})
    one_ankle = Person({**base.points, "left_ankle": (0.53, 0.9, 0.9), "right_ankle": (0.47, 0.8, 0.2)})
    tilted = base.mapped(lambda x, y: (0.5 + (x - 0.5) * 0.9 - (y - 0.5) * 0.3, 0.5 + (x - 0.5) * 0.3 + (y - 0.5) * 0.9))
    out = found + [base, feet, hand, hands, both, guess, no_head, no_hips, shoulders, unsure, one_ankle, tilted]
    turned = []
    for rotation in (90, 180, 270):
        for flips in ((False, False), (True, False)):
            rotate = lambda x, y: view_to_shown(x, y, 1.0, 0.0, 0.0, *flips, ROTATIONS[rotation])  # noqa: B023
            turned += [p.mapped(rotate) for p in (hand, feet, hands)]
    return out + turned


def boxes(rng) -> list:
    cases, people = [], poses()
    for aspect in (ASPECT, 9 / 16, 4 / 3, 1.0):
        for person in people:
            for f in (0.0, 0.3, 1.0, 1.5, 2.0, 2.7, 3.0, -1.0, 5.0):
                for hands in ((), ("left",), ("right",), ("left", "right")):
                    cases.append({"people": [record(person)], "aspect": aspect, "framing": f, "hands": list(hands),
                                  "box": target_box([person], aspect, f, set(hands)),
                                  "out": sorted(hands_out([person], aspect))})
    for _ in range(200):
        group = rng.sample(people, rng.randint(2, 4))
        f, aspect = rng.uniform(0, 3), rng.choice((ASPECT, 9 / 16, 4 / 3))
        hands = rng.choice(((), ("left",), ("left", "right")))
        cases.append({"people": [record(p) for p in group], "aspect": aspect, "framing": f, "hands": list(hands),
                      "box": target_box(group, aspect, f, set(hands)), "out": sorted(hands_out(group, aspect))})
    return [dict(c, box=list(c["box"]) if c["box"] else None) for c in cases]


def target(t: Target) -> list:
    return [t.x, t.y, t.zoom]


def a_target(rng) -> Target:
    return Target(rng.uniform(0, 1), rng.uniform(0, 1), rng.uniform(1, 4))


def fits(rng) -> list:
    cases = []
    for _ in range(300):
        x0, y0 = rng.uniform(-0.3, 1.0), rng.uniform(-0.3, 1.0)
        box = (x0, y0, x0 + rng.uniform(0, 1.2), y0 + rng.uniform(0, 1.2))
        most = rng.choice((4.0, 2.0, 1.0, 10.0))
        cases.append({"box": list(box), "max_zoom": most, "at": target(fit(box, most))})
    cases.append({"box": [0.5, 0.5, 0.5, 0.5], "max_zoom": 4.0, "at": target(fit((0.5, 0.5, 0.5, 0.5), 4.0))})
    return cases


def follows(rng) -> list:
    cases = []
    for _ in range(300):
        goal, want, scale = a_target(rng), a_target(rng), rng.uniform(0.3, 2.0)
        cases.append({"goal": target(goal), "target": target(want), "scale": scale, "at": target(follow(goal, want, scale))})
    return cases


def insides(rng) -> list:
    cases = []
    for _ in range(300):
        anchor, want, room = a_target(rng), a_target(rng), rng.uniform(0, 0.4)
        if rng.random() < 0.6:
            want = Target(anchor.x + rng.uniform(-0.1, 0.1), anchor.y + rng.uniform(-0.1, 0.1), anchor.zoom * rng.uniform(0.8, 1.2))
        cases.append({"anchor": target(anchor), "target": target(want), "room": room, "in": inside(anchor, want, room)})
    return cases


def smooths(rng) -> list:
    cases = []
    for _ in range(300):
        last, want, dt, tau = a_target(rng), a_target(rng), rng.uniform(0, 0.2), rng.choice((0.0, 0.05, 0.1, 0.3, 0.6, 2.0))
        cases.append({"last": target(last), "target": target(want), "dt": dt, "tau": tau, "at": target(smoothed(last, want, dt, tau))})
    return cases


def springs(rng) -> list:
    spring, steps = Spring(Target(0.5, 0.5, 1.0)), []
    for i in range(300):
        goal = a_target(rng) if i % 40 == 0 else goal  # noqa: F821
        dt, omega = rng.uniform(0.01, 0.2), rng.uniform(0.4, 9.0)
        steps.append({"goal": target(goal), "dt": dt, "omega": omega, "at": target(spring.step(goal, dt, omega))})
    return steps


def latches(rng) -> list:
    latch, steps, now = _Latch(0.3, 0.8), [], 0.0
    for _ in range(300):
        now += rng.uniform(0.02, 0.4)
        value = rng.random() < 0.5
        steps.append({"value": value, "now": now, "on": latch.update(value, now)})
    return steps


def rooms(rng) -> list:
    room, steps, now = _Room(), [], 0.0
    at, want = Target(0.5, 0.5, 2.0), Target(0.5, 0.5, 2.0)
    for i in range(1500):
        now += 1 / 30
        if i % 45 == 0:
            want = Target(0.5 + rng.uniform(-0.2, 0.2), 0.5 + rng.uniform(-0.1, 0.1), 2.0 * rng.uniform(0.8, 1.25))
        at = Target(at.x + (want.x - at.x) * 0.1, at.y + (want.y - at.y) * 0.1, at.zoom + (want.zoom - at.zoom) * 0.1)
        held = room.goal(want, at, 0.12, now)
        steps.append({"target": target(want), "at": target(at), "held": target(held) if held else None})
    return steps


class Clock:
    """Stands in for the time module in the plugin under test."""

    def __init__(self, now):
        self.now = now

    def monotonic(self):
        return self.now


class FramingHost:
    def __init__(self, aspect=ASPECT, owner=""):
        self._view, self._aspect = View(), aspect
        self.owner, self.wanted, self.steered = owner, set(), []

    def view(self):
        return self._view

    def view_aspect(self):
        return self._aspect

    def take_view(self, owner):
        if self.owner and self.owner != owner:
            return False
        self.owner = owner
        return True

    def release_view(self, owner):
        if self.owner == owner:
            self.owner = ""

    def steer_view(self, owner, zoom, pan_x, pan_y):
        self.steered.append([zoom, pan_x, pan_y])

    def want_people(self, owner, on):
        (self.wanted.add if on else self.wanted.discard)(owner)

    def vision_state(self):
        return vision.READY

    def focused_source_id(self):
        return "phone"

    def schedule_save(self):
        pass


def degrees(view) -> dict:
    back = {v: k for k, v in ROTATIONS.items()}
    return {"zoom": view.zoom, "pan_x": view.pan_x, "pan_y": view.pan_y, "flip_h": view.flip_h, "flip_v": view.flip_v,
            "rotation": back[view.rotation]}


class FramingRun:
    """Plays a script on the real plugin and writes down what it steered on every tick, for the Rust plugin to replay."""

    def __init__(self, name, cfg, view=None, owner=""):
        self.clock = Clock(10.0)
        framing.time = self.clock
        self.host, self.bus = FramingHost(owner=owner), EventBus()
        self.host._view = view or View()
        self.plugin = FramingPlugin()
        self.plugin.setup(self.host, self.bus)
        self.plugin.create_panel()
        self.plugin.set_config(cfg)
        self.ops = []
        self.record = {"name": name, "config": cfg, "view": degrees(self.host._view), "owner": owner, "ops": self.ops}

    def tick(self, seconds=1 / 30):
        before = len(self.host.steered)
        self.clock.now += seconds
        if self.plugin._timer.isActive():  # the real timer doesn't fire while it's off
            self.plugin._tick()
        self.ops.append({"op": "tick", "dt": seconds, "steered": self.host.steered[-1] if len(self.host.steered) > before else None})

    def people(self, people, source="phone"):
        self.bus.people_seen.emit(source, people)
        self.ops.append({"op": "people", "source": source, "people": [record(p) for p in people]})

    def run(self, seconds, people=None):
        for _ in range(round(seconds * 30)):
            self.clock.now += 1 / 30
            self.ops.append({"op": "clock", "dt": 1 / 30})
            if people is not None:
                self.people(people)
            self.tick(0.0)

    def view(self, view, aspect=None):
        self.host._view = view
        if aspect:
            self.host._aspect = aspect
        self.bus.view_changed.emit(view)
        self.ops.append({"op": "view", "view": degrees(view), "aspect": aspect})

    def max_zoom(self, most):
        self.bus.max_zoom_changed.emit(most)
        self.ops.append({"op": "max_zoom", "most": most})

    def focus(self):
        self.plugin.on_focus_gained("url", None)
        self.ops.append({"op": "focus"})

    def config(self, cfg):
        self.plugin.set_config(cfg)
        self.ops.append({"op": "config", "config": cfg})

    def done(self):
        self.record["end"] = {"config": self.plugin.get_config(), "owner": self.host.owner, "wanted": sorted(self.host.wanted),
                              "status": self.plugin._status.text(), "diagnostics": self.plugin.diagnostics()}
        self.plugin.shutdown()
        self.plugin._timer.deleteLater()
        return self.record


def wobbly(rng, person):
    return person.mapped(lambda x, y: (x + rng.uniform(-0.002, 0.002), y + rng.uniform(-0.002, 0.002)))


def framing_runs(rng) -> list:
    runs = []
    r = FramingRun("follows and goes wide", {"on": True})
    r.people([facing(x=0.2)], source="another phone")
    r.run(3, [facing(x=0.6)])
    r.run(4)
    runs.append(r.done())

    r = FramingRun("wobble in the room to move", {"on": True, "speed": 7, "room": 3, "framing": 1.4})
    for i in range(240):
        r.people([wobbly(rng, facing(x=0.5 if i < 120 else 0.62))])
        r.tick()
    runs.append(r.done())

    r = FramingRun("no room", {"on": True, "room": 0, "speed": 3, "framing": 2.5})
    r.run(4, [facing(x=0.5)])
    r.run(3, [facing(x=0.55)])
    runs.append(r.done())

    r = FramingRun("a hand held out at the calmest speed", {"on": True, "hand": True, "speed": 1})
    r.run(4, [facing()])
    r.run(1.5, [facing(left_wrist=(0.69, 0.44, 0.3), left_elbow=(0.62, 0.46, 0.3))])
    r.run(2, [facing()])
    runs.append(r.done())

    r = FramingRun("flipped", {"on": True}, view=View(flip_h=True))
    r.run(3, [facing(x=0.6)])
    runs.append(r.done())

    r = FramingRun("starts from a zoomed view", {"on": True, "speed": 10}, view=View(zoom=2.5, pan_x=0.4, pan_y=-0.3, flip_v=True))
    r.run(2, [facing(x=0.3)])
    runs.append(r.done())

    r = FramingRun("turned while tracking", {"on": True, "room": 0, "speed": 10})
    r.run(2, [facing(x=0.65)])
    r.view(replace(r.host._view, rotation=cv2.ROTATE_90_CLOCKWISE), 9 / 16)
    r.run(1)
    r.view(replace(r.host._view, rotation=None, flip_h=True), 16 / 9)
    r.run(1, [facing(x=0.65)])
    runs.append(r.done())

    r = FramingRun("max zoom", {"on": True, "framing": 0.0})
    r.run(2, [facing(width=0.05)])
    r.max_zoom(2)
    r.run(2, [facing(width=0.05)])
    r.max_zoom(10)
    r.run(1, [facing(width=0.05)])
    runs.append(r.done())

    r = FramingRun("moved to another stream", {"on": True})
    r.run(2, [facing(x=0.3)])
    r.focus()
    r.run(1)
    runs.append(r.done())

    r = FramingRun("off and on again", {"on": True})
    r.run(1, [facing(x=0.3)])
    r.config({"on": False})
    r.run(1, [facing(x=0.3)])
    r.config({"on": True, "speed": 8})
    r.run(1, [facing(x=0.3)])
    runs.append(r.done())

    r = FramingRun("something else has the view", {"on": True}, owner="Something")
    runs.append(r.done())
    return runs


def framing_configs() -> list:
    host, bus = FramingHost(), EventBus()
    plugin = FramingPlugin()
    plugin.setup(host, bus)
    plugin.create_panel()
    cases, out = [
        {}, {"on": True}, {"on": "yes"}, {"on": 1}, {"framing": 2.5, "speed": 9, "room": 0, "hand": True}, {"framing": 0}, {"framing": 3},
        {"framing": 3.5}, {"framing": -0.1}, {"framing": "head"}, {"framing": True}, {"framing": None}, {"framing": 0.05}, {"framing": 0.15},
        {"framing": 0.25}, {"framing": 0.35}, {"framing": 0.5}, {"framing": 1.25}, {"framing": 1.5}, {"framing": 2.05}, {"framing": 2.25},
        {"speed": 0}, {"speed": 1}, {"speed": 10}, {"speed": 11}, {"speed": 5.0}, {"speed": True}, {"speed": "5"}, {"room": -1}, {"room": 5},
        {"room": 6}, {"room": 2.0}, {"room": True}, {"hand": 1}, {"hand": "true"},
    ], []
    for cfg in cases:
        plugin.set_config(cfg)
        out.append({"in": cfg, "out": plugin.get_config(), "framing": plugin._framing_lbl.text(), "speed": plugin._speed_lbl.text(),
                    "room": plugin._room_lbl.text()})
    plugin.shutdown()
    plugin._timer.deleteLater()
    return out


class FaceHost:
    def __init__(self):
        self.wanted, self._view, self.streaming = set(), View(), True

    def view(self):
        return self._view

    def want_people(self, owner, on):
        (self.wanted.add if on else self.wanted.discard)(owner)

    def vision_state(self):
        return vision.READY

    def focused_source_id(self):
        return "phone"

    def is_streaming(self):
        return self.streaming

    def schedule_save(self):
        pass


class FaceRun:
    """Plays a script on the real plugin and writes down what it sent after every step."""

    def __init__(self, name):
        self.clock = Clock(100.0)
        face_exposure.time = self.clock
        self.host, self.bus = FaceHost(), EventBus()
        self.plugin = FaceExposurePlugin()
        self.plugin.setup(self.host, self.bus)
        self.plugin.create_panel()
        self.plugin.set_config({})
        self.sent = []
        self.bus.focus_point_picked.connect(lambda x, y: self.sent.append(["focus", x, y]))
        self.bus.meter_point_picked.connect(lambda x, y: self.sent.append(["meter", x, y]))
        self.ops = []
        self.record = {"name": name, "ops": self.ops}

    def op(self, name, do, **fields):
        do()
        sent, self.sent = self.sent, []
        ours = [s for s in sent if not fields.get("by_hand")]
        held = self.plugin._on and self.plugin._held()
        self.ops.append({"op": name, **fields, "sent": ours, "note": self.plugin._manual_note.text() if held else "", "held": held,
                         "status": self.plugin._status.text(), "diagnostics": self.plugin.diagnostics()["Expose for my face"]})

    def switch(self, on):
        self.op("switch", lambda: self.plugin._switch.setChecked(on), on=on)

    def see(self, tracks, seconds=1.0, source="phone"):
        for _ in range(round(seconds * 12)):
            self.clock.now += 1 / 12
            self.op("tracked", lambda: self.bus.people_tracked.emit(source, [Track(*t) for t in tracks], len(tracks)), source=source,
                    tracks=[{"id": t[0], "box": list(t[1]), "face": list(t[2]) if t[2] else None, "seen_s": t[3], "missing_s": t[4]} for t in tracks])

    def face(self, x, y, seconds=1.0, source="phone"):
        self.see([(1, (x - 0.1, y - 0.1, x + 0.1, y + 0.5), (x, y), 5.0, 0.0)], seconds, source)

    def focus_ok(self, on):
        self.op("focus_ok", lambda: self.bus.focus_point_available.emit(on), on=on)

    def meter_ok(self, on):
        self.op("meter_ok", lambda: self.bus.meter_point_available.emit(on), on=on)

    def hand(self, x, y):
        self.op("hand", lambda: self.bus.focus_point_picked.emit(x, y), x=x, y=y, by_hand=True)

    def picked(self, mode):
        self.op("picked", lambda: self.bus.focus_mode_picked.emit(mode), mode=mode)

    def mode(self, mode):
        self.op("mode", lambda: self.bus.focus_mode_changed.emit(mode), mode=mode)

    def follow(self):
        self.op("follow", self.plugin.follow_again)

    def camera(self):
        self.op("camera", lambda: self.bus.camera_switched.emit({}))

    def stop(self):
        self.op("stop", self.plugin.on_stream_stop)

    def focus(self):
        self.op("focus", lambda: self.plugin.on_focus_gained("url", None))

    def view(self, **changes):
        self.host._view = replace(self.host._view, **changes)
        self.op("view", lambda: None, view=degrees(self.host._view), stretch_x=self.host._view.stretch_x, stretch_y=self.host._view.stretch_y)

    def done(self):
        self.record["end"] = {"config": self.plugin.get_config(), "wanted": sorted(self.host.wanted)}
        self.plugin.shutdown()
        self.record["after_shutdown"] = sorted(self.host.wanted)
        return self.record


def face_runs() -> list:
    runs = []
    r = FaceRun("follows the face")
    r.focus_ok(True)
    r.face(0.4, 0.3)
    r.switch(True)
    r.face(0.4, 0.3, 2)
    r.face(0.4, 0.3, 1, source="other")
    r.face(0.7, 0.3, 3)
    r.switch(False)
    r.face(0.2, 0.3, 2)
    runs.append(r.done())

    r = FaceRun("picks you out of a crowd")
    r.focus_ok(True)
    r.switch(True)
    you, guest = (1, (0.2, 0.2, 0.4, 0.8), (0.3, 0.3), 9.0, 0.0), (2, (0.5, 0.1, 0.9, 0.9), (0.7, 0.2), 3.0, 0.0)
    r.see([guest, you], 2)
    gone = (1, (0.2, 0.2, 0.4, 0.8), (0.3, 0.3), 9.0, 1.0)
    r.see([guest, gone], 2)
    faceless = (1, (0.2, 0.2, 0.4, 0.8), None, 9.0, 0.0)
    r.see([faceless, guest], 2)
    r.see([], 1)
    runs.append(r.done())

    r = FaceRun("through the view")
    r.focus_ok(True)
    r.view(flip_h=True, zoom=2.0)
    r.switch(True)
    r.face(0.4, 0.3, 2)
    r.face(0.1, 0.5, 3)
    r.face(0.4, 0.5, 3)
    r.view(flip_h=False, zoom=1.0, stretch_x=0.5, rotation=cv2.ROTATE_90_CLOCKWISE)
    r.face(0.8, 0.8, 3)
    runs.append(r.done())

    r = FaceRun("by hand")
    r.focus_ok(True)
    r.switch(True)
    r.face(0.4, 0.3, 2)
    r.hand(0.9, 0.9)
    r.face(0.8, 0.3, 3)
    r.follow()
    r.face(0.8, 0.3, 2)
    r.picked("auto")
    r.face(0.5, 0.3, 2)
    r.follow()
    r.face(0.5, 0.3, 2)
    r.picked("manual")
    r.mode("manual")
    r.face(0.3, 0.3, 2)
    r.follow()
    r.face(0.3, 0.3, 2)
    r.mode("point")
    r.mode("manual")
    r.face(0.6, 0.3, 2)
    r.switch(False)
    r.switch(True)
    r.face(0.2, 0.3, 2)
    runs.append(r.done())

    r = FaceRun("what the lens can do")
    r.focus_ok(True)
    r.switch(True)
    r.focus_ok(False)
    r.face(0.4, 0.3, 2)
    r.meter_ok(True)
    r.face(0.4, 0.3, 3)
    r.focus_ok(False)
    r.meter_ok(True)
    r.face(0.4, 0.3, 3)
    r.camera()
    r.face(0.4, 0.3, 2)
    r.focus_ok(True)
    r.face(0.4, 0.3, 2)
    r.stop()
    r.face(0.4, 0.3, 2)
    r.focus()
    r.face(0.4, 0.3, 2)
    r.meter_ok(False)
    r.focus_ok(False)
    r.face(0.4, 0.3, 2)
    runs.append(r.done())
    return runs


def meter_runs(rng) -> list:
    runs = []
    for _ in range(12):
        meter, now, face, steps = FaceMeter(), rng.uniform(0, 5), (0.5, 0.5), []
        for _ in range(600):
            now += rng.uniform(0.02, 0.2)
            roll = rng.random()
            if roll < 0.02:
                face = (rng.uniform(0, 1), rng.uniform(0, 1))
            elif roll < 0.05:
                face = None
            elif face is None and roll < 0.2:
                face = (rng.uniform(0, 1), rng.uniform(0, 1))
            seen = (face[0] + rng.uniform(-0.01, 0.01), face[1] + rng.uniform(-0.01, 0.01)) if face else None
            point = meter.update(seen, now)
            steps.append({"face": list(seen) if seen else None, "now": now, "point": list(point) if point else None})
        runs.append(steps)
    return runs


def main():
    app = QApplication.instance() or QApplication([])
    rng = random.Random(20261030)
    (HERE / "framing.json").write_text(json.dumps({
        "tuning": [[s, *tuning(s)] for s in range(-1, 13)],
        "boxes": boxes(rng), "fits": fits(rng), "follows": follows(rng), "insides": insides(rng), "smooths": smooths(rng),
        "springs": springs(rng), "latches": latches(rng), "rooms": rooms(rng), "configs": framing_configs(), "runs": framing_runs(rng),
    }, separators=(",", ":")) + "\n")
    (HERE / "face_exposure.json").write_text(json.dumps({"meters": meter_runs(rng), "runs": face_runs()}, separators=(",", ":")) + "\n")
    del app


if __name__ == "__main__":
    main()
