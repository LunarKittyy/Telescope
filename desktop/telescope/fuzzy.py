"""Matching typed words against a few named fields of text, forgiving about typos and word endings. No Qt in here."""

import re
import unicodedata

STOP_WORDS = {"how", "to", "the", "a", "an", "my", "i", "of", "for", "in", "on", "can", "do", "does", "where", "what",
              "is", "it", "change", "set", "use", "and", "or", "with", "me", "turn", "enable", "disable", "switch",
              "toggle", "show", "find", "want", "need", "please", "setting", "settings", "option", "options", "feature",
              "not", "working", "showing", "missing", "doesn", "wont", "cant", "won", "t", "s"}
MIN_SCORE = 18.0


def normalize(text: str) -> str:
    text = unicodedata.normalize("NFKD", text.casefold())
    text = "".join(c for c in text if not unicodedata.combining(c))
    return re.sub(r"[^a-z0-9]+", " ", text).strip()


def split(text: str) -> list:
    return normalize(text).split()


def _osa(a: str, b: str) -> int:
    """Edit distance, a swap of two neighbours counting as one edit."""
    rows = [[0] * (len(b) + 1) for _ in range(len(a) + 1)]
    for i in range(len(a) + 1):
        rows[i][0] = i
    for j in range(len(b) + 1):
        rows[0][j] = j
    for i in range(1, len(a) + 1):
        for j in range(1, len(b) + 1):
            cost = a[i - 1] != b[j - 1]
            rows[i][j] = min(rows[i - 1][j] + 1, rows[i][j - 1] + 1, rows[i - 1][j - 1] + cost)
            if i > 1 and j > 1 and a[i - 1] == b[j - 2] and a[i - 2] == b[j - 1]:
                rows[i][j] = min(rows[i][j], rows[i - 2][j - 2] + 1)
    return rows[-1][-1]


def _subsequence(q: str, text: str) -> bool:
    it = iter(text)
    return all(c in it for c in q)


def _word_score(q: str, w: str) -> float:
    """How well the typed word q fits one word of a field, 0 for not at all."""
    if w == q:
        return 100.0
    if w.startswith(q):
        return 88.0 - min(12, len(w) - len(q))
    best = 0.0
    if len(q) >= 5 and q in w:
        best = 55.0  # inside a longer word (v4l2loopback); shorter ones would find "crop" in "microphone"
    if len(q) >= 4 and q[0] == w[0]:  # slips are rarely in the first letter, and 4 letter words are too close to each other
        allowed = 1 if len(q) < 7 else 2
        sizes = (len(q),) if len(q) == 4 else (len(q) - 1, len(q), len(q) + 1)
        d = min(_osa(q, w[:k]) for k in sizes)
        if d <= allowed:
            best = max(best, 74.0 - 22.0 * d)
    return best


class Doc:
    """Named fields of text, ready to be scored against what someone typed."""

    def __init__(self, **fields: str):
        self._fields = {}
        for key, text in fields.items():
            words = split(text or "")
            self._fields[key] = (words, "".join(words), "".join(w[0] for w in words))

    def _field_score(self, token: str, key: str) -> float:  # noqa: C901
        words, joined, initials = self._fields[key]
        best = max((_word_score(token, w) for w in words), default=0.0)
        if best < 60 and 2 <= len(token) <= 6 and len(words) > 1 and initials.startswith(token):
            best = max(best, 66.0)  # "ha" for Hardware acceleration
        if best < 40 and key == "name" and len(token) >= 5 and token[0] == joined[:1] and _subsequence(token, joined):
            best = max(best, 34.0)  # "hwaccel" for Hardware acceleration
        return best

    def score(self, query: str, weights: dict, whole: str = "name", partial: bool = False) -> float:
        """0 when a word of the query matches nothing (a one or two letter word is let off), else higher is better.
        partial: words that match nothing only pull the score down instead of ruling the field out."""
        tokens = split(query)
        if len(tokens) > 1:
            tokens = [t for t in tokens if t not in STOP_WORDS] or tokens
        total, used, counted = 0.0, 0, 0
        for token in tokens:
            best = max((w * self._field_score(token, k) for k, w in weights.items() if k in self._fields), default=0.0)
            if best <= 0:
                if len(token) <= 2:
                    continue
                if not partial:
                    return 0.0
                counted += 1
                continue
            total += best
            used += 1
            counted += 1
        if not used:
            return 0.0
        result = total / counted
        phrase = normalize(query)
        name = " ".join(self._fields[whole][0]) if whole in self._fields else ""
        if phrase and name:
            if phrase == name:
                result += 40
            elif name.startswith(phrase):
                result += 16
            elif phrase in name:
                result += 10
        return result if result >= MIN_SCORE else 0.0
