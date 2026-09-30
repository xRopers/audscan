"""Write docs/demo.bin: a made-up archive for the README's screenshots.

It holds synthesized sounds only (no game audio): a WAV chime, a Wwise SoundBank of three
PCM WEMs, and an FMOD FSB5 bank of three named PCM tracks, between filler bytes.

    python docs/make_demo.py
    audscan-gui docs/demo.bin
"""
import math
import random
import struct
from pathlib import Path

RATE = 44100
random.seed(7)


def pcm16(samples):
    return b''.join(struct.pack('<h', max(-32768, min(32767, int(v * 32767)))) for v in samples)


def tone(seconds, partials, decay, stereo=False):
    """Decaying partials [(freq, amp)], with a slight stereo spread."""
    out = []
    for i in range(int(seconds * RATE)):
        t = i / RATE
        env = math.exp(-t * decay) * min(1.0, t * 200)
        v = sum(a * math.sin(2 * math.pi * f * t) for f, a in partials) * env
        out += [v * 0.9, v * 0.8] if stereo else [v]
    return out


def burst(seconds, color, decay):
    """Filtered noise with an envelope: footsteps, doors."""
    out, y = [], 0.0
    for i in range(int(seconds * RATE)):
        t = i / RATE
        y += (random.uniform(-1, 1) - y) * color
        out.append(y * math.exp(-t * decay) * 2.5)
    return out


def ambience(seconds):
    out, y = [], 0.0
    for i in range(int(seconds * RATE)):
        t = i / RATE
        y += (random.uniform(-1, 1) - y) * 0.02
        swell = 0.5 + 0.5 * math.sin(2 * math.pi * t / seconds * 2)
        out.append((y * 4 + 0.15 * math.sin(2 * math.pi * 110 * t)) * (0.3 + 0.5 * swell))
    return out


def chunk(tag, body):
    return tag + struct.pack('<I', len(body)) + body + (b'\0' if len(body) % 2 else b'')


def wav(samples, channels, extra=b''):
    fmt = struct.pack('<HHIIHH', 1, channels, RATE, RATE * 2 * channels, 2 * channels, 16)
    body = b'WAVE' + chunk(b'fmt ', fmt) + extra + chunk(b'data', pcm16(samples))
    return b'RIFF' + struct.pack('<I', len(body)) + body


def bnk(media):
    """A SoundBank: BKHD, DIDX, DATA (16-byte aligned), HIRC."""
    index, data = b'', b''
    for wem_id, wem in media:
        data += b'\0' * (-len(data) % 16)
        index += struct.pack('<III', wem_id, len(data), len(wem))
        data += wem
    body = [(b'BKHD', struct.pack('<II', 0x88, 0x5EED) + b'\0' * 8), (b'DIDX', index), (b'DATA', data), (b'HIRC', b'\0' * 24)]
    return b''.join(tag + struct.pack('<I', len(b)) + b for tag, b in body)


def fsb5(tracks):
    """An FSB5 bank of 16-bit PCM tracks [(name, channels, samples)], 44.1 kHz."""
    headers, data, names, strings = b'', b'', b'', b''
    for name, channels, samples in tracks:
        data += b'\0' * (-len(data) % 32)
        frames = len(samples) // channels
        packed = (8 << 1) | ((channels - 1) << 5) | ((len(data) // 32) << 7) | (frames << 34)
        headers += struct.pack('<Q', packed)
        names += struct.pack('<I', 4 * len(tracks) + len(strings))
        strings += name.encode() + b'\0'
        data += pcm16(samples)
    names += strings
    names += b'\0' * (-len(names) % 4)
    head = b'FSB5' + struct.pack('<IIIIII', 1, len(tracks), len(headers), len(names), len(data), 2)
    head += b'\0' * (0x3C - len(head))
    return head + headers + names + data


def filler(n):
    return bytes(random.getrandbits(8) for _ in range(n))


chime = wav(tone(2.2, [(523.25, 0.4), (784.0, 0.25), (1046.5, 0.15), (1568.0, 0.08)], 2.2, stereo=True), 2)
akd = chunk(b'akd ', b'\0' * 16)
wwise = bnk([
    (100381, wav(tone(0.8, [(880, 0.5), (1320, 0.2)], 6), 1, akd)),
    (100382, wav(burst(0.5, 0.3, 9), 1, akd)),
    (100383, wav(tone(1.2, [(220, 0.5), (330, 0.3), (440, 0.2)], 3), 1, akd)),
])
fmod = fsb5([
    ('door_open', 1, burst(0.9, 0.05, 4) + tone(0.3, [(90, 0.6)], 12)),
    ('footstep_01', 1, burst(0.35, 0.5, 14)),
    ('ambience_loop', 1, ambience(4.0)),
])
archive = filler(0x800) + chime + filler(0x333) + wwise + filler(0x555) + fmod + filler(0x400)
out = Path(__file__).with_name('demo.bin')
out.write_bytes(archive)
print(f'{out}: {len(archive)} bytes')
