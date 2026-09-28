"""Verify that each DTMF method reaches the other phone through the PBX: RFC 4733 events and SIP INFO arrive as digits, and in-band digits arrive as their two tones for the length KSIP sends them."""
import math, struct, time, wave
from sip_fixture import Phone, ROOT, accounts, connect

DIGITS = '159*#0'
# The two frequencies of each digit (ITU-T Q.23).
LOWS, HIGHS = (697, 770, 852, 941), (1209, 1336, 1477, 1633)
KEYS = '123A456B789C*0#D'


def power(samples, frequency, rate):
    """Goertzel: the power of one frequency in a run of samples."""
    k = 2 * math.cos(2 * math.pi * frequency / rate)
    s1 = s2 = 0.0
    for x in samples:
        s1, s2 = x + k * s1 - s2, s1
    return s1 * s1 + s2 * s2 - k * s1 * s2


def tones(path):
    """The digits heard in a recording, each with how long it sounded (ms):
    30 ms windows (fine enough to tell 697 Hz from 770 Hz) every 10 ms, a
    window being a digit when one low and one high frequency stand well
    above the other six."""
    with wave.open(str(path), 'rb') as w:
        rate, channels, frames = w.getframerate(), w.getnchannels(), w.readframes(w.getnframes())
    samples = struct.unpack('<%dh' % (len(frames) // 2), frames)[::channels]
    step, width = rate // 100, rate * 30 // 1000
    heard = []
    for start in range(0, len(samples) - width, step):
        window = samples[start:start + width]
        if max(abs(x) for x in window) < 500:
            heard.append(None)
            continue
        lows = [power(window, f, rate) for f in LOWS]
        highs = [power(window, f, rate) for f in HIGHS]
        low, high = max(range(4), key=lambda i: lows[i]), max(range(4), key=lambda i: highs[i])
        rest = max([p for i, p in enumerate(lows) if i != low] + [p for i, p in enumerate(highs) if i != high])
        clear = lows[low] > 10 * rest and highs[high] > 10 * rest
        heard.append(KEYS[low * 4 + high] if clear else None)
    runs = []
    for digit in heard:
        if runs and runs[-1][0] == digit:
            runs[-1][1] += 10
        else:
            runs.append([digit, 10])
    return [(d, ms) for d, ms in runs if d is not None and ms >= 30]


def main():
    configured = accounts()
    heard_file = ROOT / 'temp/build/ksip-integration/dtmf-b/heard.wav'
    heard_file.parent.mkdir(parents=True, exist_ok=True)
    heard_file.unlink(missing_ok=True)
    a = b = None
    try:
        a = Phone('dtmf-a', configured[0])
        b = Phone('dtmf-b', configured[1], audio_player=f'aufile,{heard_file.as_posix()}')
        call, _ = connect(a, b, configured[1]['extension'])
        for mode in ('rtp', 'info'):
            before = len(b.events)
            for digit in DIGITS:
                a.action('dtmf', call, digit, mode=mode)
                time.sleep(0.3)
            end = time.monotonic() + 5
            got = ''
            while time.monotonic() < end:
                got = ''.join(e.get('param', '') for e in b.events[before:] if e.get('type') == 'CALL_DTMF_START')
                if got == DIGITS:
                    break
                time.sleep(0.2)
            assert got == DIGITS, f'{mode}: the other phone got {got!r}, not {DIGITS!r}'
            print(f'PASS: {mode} の DTMF が PBX を越えて数字として届いた（{got}）')
        # In band: queued at once, sounded one after another. KSIP sends each
        # tone for 100 ms (test-ksip-audio holds it to that); what arrives can
        # be a little shorter where the far side's jitter buffer drops or
        # stretches a packet, so the check is the rule's own minimum, 50 ms.
        for digit in DIGITS:
            a.action('dtmf', call, digit, mode='inband')
        time.sleep(len(DIGITS) * 0.2 + 1.5)
        a.action('hangup', call)
        b.wait(lambda s: not s['calls'])
        b.close()
        b = None
        found = tones(heard_file)
        digits = ''.join(d for d, _ in found)
        assert digits == DIGITS, f'inband: the other phone heard {found}, not {DIGITS!r}'
        assert all(50 <= ms <= 130 for _, ms in found), f'inband: tone lengths {found}'
        print(f'PASS: 帯域内の DTMF が音として届き、1桁ずつ50ms以上鳴った（{found}）')
    finally:
        if a:
            a.close()
        if b:
            b.close()


if __name__ == '__main__':
    main()
