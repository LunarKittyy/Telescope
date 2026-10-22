"""What a pose says about someone, shared by the plugins that look at people: Auto framing and the Preview's skeleton.
Points are (x, y) in the picture as shown at zoom 1, as vision.Person has them."""
import math
from typing import Optional

SEEN = 0.5  # landmark visibility the model has to be this sure of
ARM_SEEN = 0.15  # arms count at lower visibility: past the edge of the picture the model only guesses where they are
HAND_OUT = 0.9  # a wrist this many shoulder widths out past its shoulder is holding something out
ELBOW_OUT = 0.55  # or an elbow this far out with the wrist further still (the hand itself can be out of the picture)
SIDES = ("left", "right")

# The lines of a skeleton, between vision.Person's point names
BONES = (
    ("left_ear", "left_eye"), ("left_eye", "nose"), ("nose", "right_eye"), ("right_eye", "right_ear"),
    ("left_shoulder", "right_shoulder"), ("left_hip", "right_hip"),
    ("left_shoulder", "left_hip"), ("right_shoulder", "right_hip"),
) + tuple(pair for side in SIDES for pair in (
    (f"{side}_shoulder", f"{side}_elbow"), (f"{side}_elbow", f"{side}_wrist"),
    (f"{side}_wrist", f"{side}_index"), (f"{side}_wrist", f"{side}_pinky"), (f"{side}_wrist", f"{side}_thumb"),
))


def _mid(a, b):
    return ((a[0] + b[0]) / 2, (a[1] + b[1]) / 2)


def body(person, aspect: float) -> Optional[tuple]:
    """(shoulder centre, shoulder width in picture heights) or None without both shoulders."""
    left, right = person.point("left_shoulder", SEEN), person.point("right_shoulder", SEEN)
    if left is None or right is None:
        return None
    width = math.hypot((left[0] - right[0]) * aspect, left[1] - right[1])
    return (_mid(left, right), width) if width > 1e-3 else None


def arm(person, side: str, aspect: float) -> Optional[tuple]:
    """(shoulder width, how far out past the shoulder a point is, the hip line) for one arm, or None."""
    shoulders = body(person, aspect)
    shoulder = person.point(f"{side}_shoulder", SEEN)
    if shoulders is None or shoulder is None:
        return None
    (cx, sy), width = shoulders
    away = 1.0 if shoulder[0] >= cx else -1.0

    def out(p):
        return (p[0] - shoulder[0]) * away * aspect
    hip = person.point(f"{side}_hip", SEEN)
    return width, out, (hip[1] - 0.2 * width) if hip else sy + 1.4 * width


def hands_out(people, aspect: float) -> set:
    """The sides ("left", "right") on which someone holds a hand out (showing something), above their hips."""
    found = set()
    for person in people:
        for side in SIDES:
            found_arm = arm(person, side, aspect)
            if found_arm is None:
                continue
            width, out, below = found_arm
            elbow, wrist = person.point(f"{side}_elbow", ARM_SEEN), person.point(f"{side}_wrist", ARM_SEEN)
            if wrist is None or wrist[1] > below:
                continue
            if out(wrist) > HAND_OUT * width or (elbow and out(elbow) > ELBOW_OUT * width and out(wrist) > out(elbow)):
                found.add(side)
    return found
