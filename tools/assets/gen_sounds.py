#!/usr/bin/env python3
"""Synthesises the justOS system sounds.

"avvio" is the chime played when the desktop appears: three soft bell
notes rising (an add9 arpeggio on D), each a few sine partials with a quick
attack and an exponential decay, plus a little room reverb.

The three songs in the Musica folder are made the same way: chords on a
soft pad, a bass line, a bell arpeggio and (in two of them) a light beat.

Output: <folder>/avvio.wav and, with a second folder, the songs in it
(22050 Hz, 16-bit mono).

Usage: tools/assets/gen_sounds.py [sounds folder] [music folder]
"""
import os
import sys
import wave

import numpy as np

RATE = 22050


def bell(freq, length):
    t = np.arange(int(RATE * length)) / RATE
    tone = np.zeros_like(t)
    for mult, amp, decay in ((1.0, 1.0, 2.2), (2.0, 0.35, 3.5), (3.0, 0.12, 5.0), (4.2, 0.05, 7.0)):
        tone += amp * np.sin(2 * np.pi * freq * mult * t) * np.exp(-decay * t)
    attack = np.minimum(t / 0.012, 1.0)
    return tone * attack


def chime():
    length = 1.9
    out = np.zeros(int(RATE * length))
    # D5, A5, E6 (D add9): bright but calm.
    for start, freq, gain in ((0.0, 587.33, 0.55), (0.14, 880.0, 0.45), (0.28, 1318.5, 0.38)):
        b = bell(freq, length - start) * gain
        i = int(start * RATE)
        out[i:i + len(b)] += b
    # A few early reflections for some space.
    room = out.copy()
    for delay, amp in ((0.031, 0.25), (0.057, 0.18), (0.089, 0.12), (0.131, 0.08)):
        d = int(delay * RATE)
        room[d:] += out[:-d] * amp
    room /= np.max(np.abs(room))
    fade = np.minimum(np.arange(len(room))[::-1] / (0.25 * RATE), 1.0)
    return (room * fade * 0.85 * 32767).astype("<i2")


NOTES = {"C": 0, "D": 2, "E": 4, "F": 5, "G": 7, "A": 9, "B": 11}


def hz(name):
    """'A4' -> 440 Hz; sharps as 'C#4'."""
    n = NOTES[name[0]] + (1 if "#" in name else 0)
    octave = int(name[-1])
    return 440.0 * 2 ** ((n - 9) / 12 + (octave - 4))


def pad(freqs, length):
    t = np.arange(int(RATE * length)) / RATE
    out = np.zeros_like(t)
    for f in freqs:
        for det in (-0.6, 0.6):
            out += np.sin(2 * np.pi * (f + det) * t) + 0.25 * np.sin(2 * np.pi * 2 * (f + det) * t)
    env = np.minimum(t / 0.6, 1.0) * np.minimum((length - t) / 0.5, 1.0)
    return out * env / (len(freqs) * 2)


def pluck(freq, length, bright=1.0):
    t = np.arange(int(RATE * length)) / RATE
    tone = np.sin(2 * np.pi * freq * t) + 0.3 * bright * np.sin(2 * np.pi * 2 * freq * t) + 0.1 * bright * np.sin(2 * np.pi * 3 * freq * t)
    return tone * np.exp(-4.5 * t) * np.minimum(t / 0.004, 1.0)


def bass(freq, length):
    t = np.arange(int(RATE * length)) / RATE
    tone = np.sin(2 * np.pi * freq * t) + 0.2 * np.sin(2 * np.pi * 2 * freq * t)
    return tone * np.exp(-1.6 * t) * np.minimum(t / 0.01, 1.0) * np.minimum((length - t) / 0.05, 1.0)


def kick():
    t = np.arange(int(RATE * 0.3)) / RATE
    f = 45 + 90 * np.exp(-30 * t)
    return np.sin(2 * np.pi * np.cumsum(f) / RATE) * np.exp(-9 * t)


def hat(rng):
    n = int(RATE * 0.05)
    noise = rng.standard_normal(n)
    noise = np.diff(noise, prepend=0)  # brighter
    return noise * np.exp(-np.arange(n) / (RATE * 0.012)) * 0.25


def song(chords, bpm, bars_per_chord, repeats, arp, beat, seed):
    """chords: lists of note names (lowest is the bass)."""
    rng = np.random.default_rng(seed)
    beat_s = 60 / bpm
    bar = 4 * beat_s
    total = len(chords) * bars_per_chord * bar * repeats + 2.5
    out = np.zeros(int(RATE * total))

    def add(sig, at, gain):
        i = int(at * RATE)
        j = min(len(out), i + len(sig))
        out[i:j] += sig[: j - i] * gain

    at = 0.0
    for r in range(repeats):
        for ch in chords:
            length = bars_per_chord * bar
            freqs = [hz(n) for n in ch]
            add(pad(freqs[1:], length + 0.4), at, 0.32)
            for b in range(bars_per_chord * 4):
                add(bass(freqs[0] / 2, beat_s * 0.95), at + b * beat_s, 0.45 if b % 2 == 0 else 0.3)
            if arp:
                steps = bars_per_chord * 8
                seq = freqs[1:] + [f * 2 for f in freqs[1:]]
                for k in range(steps):
                    f = seq[(k * 3 + r) % len(seq)] * 2
                    add(pluck(f, beat_s * 1.6, 0.8), at + k * beat_s / 2, 0.16 if k % 2 else 0.22)
            if beat and r > 0:
                for b in range(bars_per_chord * 4):
                    t0 = at + b * beat_s
                    if b % 2 == 0:
                        add(kick(), t0, 0.55)
                    add(hat(rng), t0 + beat_s / 2, 1.0)
                    if b % 4 == 3:
                        add(hat(rng), t0 + beat_s * 0.75, 0.6)
            at += length
    # Space: a few echoes, then normalise and fade.
    room = out.copy()
    for delay, amp in ((0.083, 0.22), (0.161, 0.14), (0.277, 0.08)):
        d = int(delay * RATE)
        room[d:] += out[:-d] * amp
    room /= np.max(np.abs(room))
    n = len(room)
    fade = np.minimum(np.minimum(np.arange(n) / (0.8 * RATE), np.arange(n)[::-1] / (2.5 * RATE)), 1.0)
    return (room * fade * 0.8 * 32767).astype("<i2")


SONGS = [
    ("Alba sulle Dolomiti", [["C3", "E4", "G4", "C5"], ["G2", "D4", "G4", "B4"], ["A2", "C4", "E4", "A4"], ["F2", "C4", "F4", "A4"]], 76, 2, 1, True, False),
    ("Notte in città", [["A2", "C4", "E4", "G4"], ["D3", "C4", "F4", "A4"], ["G2", "B3", "D4", "F4"], ["C3", "B3", "E4", "G4"]], 84, 2, 2, True, True),
    ("Arco", [["F2", "A3", "C4", "F4"], ["G2", "B3", "D4", "G4"], ["E2", "G3", "B3", "E4"], ["A2", "A3", "C4", "E4"]], 112, 2, 2, True, True),
]


def save(path, samples):
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(samples.tobytes())


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "rootfs/system/sounds"
    os.makedirs(out, exist_ok=True)
    save(os.path.join(out, "avvio.wav"), chime())
    if len(sys.argv) > 2:
        music = sys.argv[2]
        os.makedirs(music, exist_ok=True)
        for i, (name, chords, bpm, bars, reps, arp, beat) in enumerate(SONGS):
            save(os.path.join(music, name + ".wav"), song(chords, bpm, bars, reps, arp, beat, i))


if __name__ == "__main__":
    main()
