# Phone states for the Camera card, as lines for `telescope-rs scenes OUT --card camera_control --window --config-lines`.
# python states.py lenses > lenses-states.jsonl      python states.py pro pro-map.json > pro-states.jsonl
import json, math, sys
sizes = [(3840, 2160), (1920, 1080), (1280, 720), (854, 480), (640, 480), (1440, 1080)]
def lens(id="0", current=True, **fields):
    cam = {"id": id, "current": current, "label": "Back ~24mm OIS", "facing": "back", "focalMm": 24, "hasOis": True,
           "hwLevel": "FULL", "supportsManualSensor": True, "supportsManualWB": True, "supportsManualFocus": True,
           "supportsFocusPoint": True, "supportsFlash": True, "minFocusDistance": 10.0, "isoMin": 50, "isoMax": 3200,
           "shutterMinNs": 100_000, "shutterMaxNs": 500_000_000, "aeCompMin": -8, "aeCompMax": 8, "aeCompStep": 0.167,
           "maxFps": 60, "supportedSizes": [{"width": w, "height": h} for w, h in sizes],
           "h264Sizes": [{"width": w, "height": h} for w, h in sizes[:-1]], "zoomRatioMax": 1.0}
    cam.update(fields); return cam
def state(current, **extra):
    cams = [lens("2", current == "2", label="Back ~13mm", focalMm=13, hasOis=False),
            lens("0", current == "0", zoomRatioMax=8.0, lensZooms=[5.0]),
            lens("4", current == "4", label="Back ~113mm", focalMm=113)]
    s = {"cameras": cams, "auto": True, "wb_manual": False, "ois": True, "focus_mode": "continuous", "focus_distance": 0.0, "ae_comp": 0, "nr_mode": 1,
         "edge_mode": 1, "black_level_lock": False, "torch": False, "battery": 64, "charging": False, "battery_temp_c": 30.0, "codecs": ["h264", "mjpeg"],
         "codec": "h264", "dynamic_bitrate": True, "stream_width": 1920, "stream_height": 1080}
    s.update(extra); return s
PRO = dict(auto=False, iso=400, shutter_ns=8_000_000, wb_manual=True, focus_mode="manual", focus_distance=2.0)
def clamp(v, a=0.0, b=1.0): return max(a, min(b, v))
def io5(t): t = clamp(t); return t * t * t * (t * (6 * t - 15) + 10)
def seg(t, keys):
    if t <= keys[0][0]: return keys[0][1]
    for (t0, v0), (t1, v1) in zip(keys, keys[1:]):
        if t < t1: return v0 + (v1 - v0) * io5((t - t0) / (t1 - t0))
    return keys[-1][1]
ISO = [[0, math.log(400)], [1.55, math.log(400)], [2.3, math.log(1600)], [2.6, math.log(1600)], [3.4, math.log(100)], [3.65, math.log(100)], [4.05, math.log(400)]]
KEL = [[0, 5500], [4.7, 5500], [5.4, 7600], [5.65, 7600], [6.45, 3400], [6.7, 3400], [7.2, 5500]]
DIO = [[0, 2.0], [7.9, 2.0], [8.7, 0.0], [9.0, 0.0], [9.75, 2.0]]
def pro_values(f):
    t = f / 60
    return round(math.exp(seg(t, ISO))), int(round(seg(t, KEL) / 10) * 10), round(seg(t, DIO) / 10.0 * 1000) / 1000 * 10
def main():
    if sys.argv[1] == "lenses":
        for c in ("2", "0", "4"):
            print(json.dumps({"_state": state(c)}))
    else:
        # one line per distinct card state; frames.json maps each frame to its line
        seen, lines, frames = {}, [], []
        for f in range(round(10.5 * 60)):
            key = pro_values(f)
            if key not in seen:
                seen[key] = len(lines)
                iso, k, d = key
                lines.append(json.dumps({"wb_kelvin": k, "_props": {"kelvin": k, "kelvin-text": f"{k} K"}, "_state": state("0", **{**PRO, "iso": iso, "focus_distance": d})}))
            frames.append(seen[key])
        print("\n".join(lines))
        json.dump(frames, open(sys.argv[2], "w"))

if __name__ == "__main__":
    main()
