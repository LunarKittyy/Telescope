"""Calm people tracking on top of vision's people: steady ids, smoothed boxes and faces, and a people count that's
quick to notice someone new and slow to let them go, since the pose model drops people and finds them again all the
time. No Qt here."""
import math
from dataclasses import dataclass
from typing import Callable, Optional

SEEN = 0.5  # a landmark counts at this visibility
CONFIRM = 2  # a new person counts once found in this many updates in a row
DROP_S = 3.0  # someone counted is only let go after being missing this long
SMOOTH_S = 0.25  # boxes and faces follow the model with this time constant
GATE = 1.2  # a person found again within this many of their own sizes of where they were going is the same person
GATE_GROWTH = 1.5  # and the reach grows by this many sizes a second while they're missing
GATE_MOST = 4.0
FORGET_S = 60.0  # a stream with no update for this long is forgotten
DUPLICATE = 0.6  # a second skeleton overlapping someone found this much (intersection over union) is them again
_FACE = ("nose", "left_eye", "right_eye", "left_ear", "right_ear")


@dataclass(frozen=True)
class Track:
    """One person followed across updates, in the picture as shown at zoom 1 (as vision.Person has it)."""
    id: int
    box: tuple
    """(x0, y0, x1, y1) around the landmarks seen, smoothed."""
    face: Optional[tuple]
    """(x, y) of the middle of the face, smoothed; None until a face point has been seen."""
    seen_s: float
    """How long since they were first found."""
    missing_s: float
    """How long since they were last found; 0 while they're found."""

    @property
    def centre(self) -> tuple:
        return (self.box[0] + self.box[2]) / 2, (self.box[1] + self.box[3]) / 2


def box_of(person) -> Optional[tuple]:
    """(x0, y0, x1, y1) around the landmarks the model is sure of, or None with fewer than two."""
    points = [p for p in (person.point(name, SEEN) for name in person.points) if p]
    if len(points) < 2:
        return None
    xs, ys = [p[0] for p in points], [p[1] for p in points]
    return min(xs), min(ys), max(xs), max(ys)


def face_of(person) -> Optional[tuple]:
    """The middle of the face points the model is sure of, or None."""
    points = [p for p in (person.point(name, SEEN) for name in _FACE) if p]
    if not points:
        return None
    return sum(p[0] for p in points) / len(points), sum(p[1] for p in points) / len(points)


def main(tracks: list) -> Optional[Track]:
    """Whoever's been there longest (you, usually): the one a plugin follows, and the one it doesn't blur."""
    if not tracks:
        return None
    return max(tracks, key=lambda t: (t.seen_s, _area(t.box)))


def _area(box: tuple) -> float:
    return max(box[2] - box[0], 0.0) * max(box[3] - box[1], 0.0)


def _overlap(a: tuple, b: tuple) -> float:
    inter = _area((max(a[0], b[0]), max(a[1], b[1]), min(a[2], b[2]), min(a[3], b[3])))
    union = _area(a) + _area(b) - inter
    return inter / union if union > 0 else 0.0


def _size(box: tuple) -> float:
    return max(box[2] - box[0], box[3] - box[1], 0.02)


def _centre(box: tuple) -> tuple:
    return (box[0] + box[2]) / 2, (box[1] + box[3]) / 2


def _ease(old, new, a: float):
    return tuple(o + (n - o) * a for o, n in zip(old, new))


class _Follow:
    def __init__(self, id: int, box: tuple, face: Optional[tuple], now: float):
        self.id, self.box, self.face = id, box, face
        self.velocity = (0.0, 0.0)
        self.first_at = self.last_at = now
        self.hits = 1
        self.counted = False

    def predicted(self, now: float) -> tuple:
        cx, cy = _centre(self.box)
        ahead = min(now - self.last_at, 1.0)
        return cx + self.velocity[0] * ahead, cy + self.velocity[1] * ahead

    def found(self, box: tuple, face: Optional[tuple], now: float, missed: bool):
        dt = max(now - self.last_at, 1e-3)
        a = 1.0 - math.exp(-dt / SMOOTH_S)
        before = _centre(self.box)
        self.box = _ease(self.box, box, a)
        if face is not None:
            self.face = face if self.face is None else _ease(self.face, face, a)
        moved = _centre(self.box)
        step = ((moved[0] - before[0]) / dt, (moved[1] - before[1]) / dt)
        self.velocity = _ease(self.velocity, step, 0.5)
        self.last_at = now
        self.hits = 1 if missed else self.hits + 1
        if self.hits >= CONFIRM:
            self.counted = True

    def mapped(self, to: Callable):
        corners = [to(x, y) for x in (self.box[0], self.box[2]) for y in (self.box[1], self.box[3])]
        self.box = (min(p[0] for p in corners), min(p[1] for p in corners),
                    max(p[0] for p in corners), max(p[1] for p in corners))
        self.face = to(*self.face) if self.face is not None else None
        self.velocity = (0.0, 0.0)

    def track(self, now: float) -> Track:
        return Track(self.id, self.box, self.face, now - self.first_at, now - self.last_at)


class PeopleTracker:
    """One stream's people, from each update's [vision.Person]."""

    def __init__(self):
        self._follows: list = []
        self._next_id = 1
        self._updated_at: Optional[float] = None

    def update(self, people: list, now: float) -> list:
        """The people counted after this update, as [Track]. Someone new counts once found CONFIRM updates in a row;
        someone counted stays (missing_s growing) until missing for DROP_S."""
        found = [(box, face_of(p)) for p in people if (box := box_of(p)) is not None]
        pairs = []
        for i, f in enumerate(self._follows):
            px, py = f.predicted(now)
            reach = min(GATE + GATE_GROWTH * (now - f.last_at), GATE_MOST) * _size(f.box)
            for j, (box, _face) in enumerate(found):
                cx, cy = _centre(box)
                distance = math.hypot(cx - px, cy - py)
                if distance <= reach:
                    pairs.append((distance / _size(f.box), not f.counted, i, j))
        pairs.sort()  # nearest first, and someone already counted before someone new at the same distance
        taken_f, taken_p = set(), set()
        for _cost, _new, i, j in pairs:
            if i in taken_f or j in taken_p:
                continue
            taken_f.add(i)
            taken_p.add(j)
            f = self._follows[i]
            missed = self._updated_at is not None and f.last_at < self._updated_at
            f.found(*found[j], now, missed)
        kept = []
        for i, f in enumerate(self._follows):
            if i in taken_f or (f.counted and now - f.last_at < DROP_S):
                kept.append(f)
        claimed = [found[j][0] for j in taken_p]
        for j, (box, face) in enumerate(found):
            if j not in taken_p and not any(_overlap(box, other) > DUPLICATE for other in claimed):
                claimed.append(box)
                kept.append(_Follow(self._next_id, box, face, now))
                self._next_id += 1
        self._follows = kept
        self._updated_at = now
        return self.tracks(now)

    def tracks(self, now: float) -> list:
        return [f.track(now) for f in self._follows if f.counted]

    def count(self) -> int:
        """How many people there are, steadied."""
        return sum(1 for f in self._follows if f.counted)

    def remap(self, to: Callable):
        """Everyone's position put through to(x, y): the picture turned or flipped, and they're the same people."""
        for f in self._follows:
            f.mapped(to)

    def reset(self):
        self._follows, self._updated_at = [], None


class PeopleTracking:
    """A PeopleTracker per stream, by source id."""

    def __init__(self):
        self._trackers: dict = {}
        self._last: dict = {}  # source id: ([Track], count, when)

    def update(self, source_id: str, people: list, now: float) -> tuple:
        """([Track], count) for source_id after this update."""
        for sid in [s for s, (_t, _c, at) in self._last.items() if now - at > FORGET_S]:
            self.drop(sid)
        tracker = self._trackers.setdefault(source_id, PeopleTracker())
        tracks = tracker.update(people, now)
        self._last[source_id] = (tracks, tracker.count(), now)
        return tracks, tracker.count()

    def last(self, source_id: str, now: float) -> tuple:
        """What the last update for source_id gave, or ([], 0) without one in DROP_S (nothing asks for people, or
        the stream stopped)."""
        tracks, count, at = self._last.get(source_id, ([], 0, -math.inf))
        return (tracks, count) if now - at <= DROP_S else ([], 0)

    def drop(self, source_id: str):
        """source_id's stream ended."""
        self._trackers.pop(source_id, None)
        self._last.pop(source_id, None)

    def remap(self, to: Callable):
        """Everyone on every stream put through to(x, y), keeping their ids (the picture turned or flipped)."""
        for sid, tracker in self._trackers.items():
            tracker.remap(to)
            if sid in self._last:
                _tracks, count, at = self._last[sid]
                self._last[sid] = (tracker.tracks(at), count, at)
