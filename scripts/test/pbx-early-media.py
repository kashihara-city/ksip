"""Early media against the lab PBX: a 183 with audio before the answer must neither leak into a recording nor break the recording once the call is answered. Runs only where lab.json names an early_media number."""
import time
import wave

from sip_fixture import PBX, Phone, accounts, connect, numbers


def main():
    number = numbers().get('early_media')
    if not number:
        print(f'SKIP: アーリーメディアを流す番号が無い（{PBX} の lab.json の numbers に early_media が無い）', flush=True)
        return
    configured = accounts()

    # One call on its own: the state passes through EARLY, the decoder exists
    # before the answer, and a recording started after the answer takes the
    # audio at once, although the stream was made for the 183.
    a = None
    try:
        a = Phone('early-a', configured[0], 18590, 19290)
        call = a.action('dial', value=number)
        a.wait(lambda s: any(c['id'] == call and c['state'] == 'EARLY' for c in s['calls']), timeout=15)
        print('PASS: 183で通話がEARLYになる', flush=True)
        time.sleep(1)
        text = a.log_text()
        assert 'Set audio decoder' in text, 'no decoder was set during early media'
        assert 'Call established' not in text, 'the call was answered before the check'
        print('PASS: 応答前に音声のデコーダーが立つ', flush=True)
        a.wait(lambda s: any(c['id'] == call and c['state'] == 'ESTABLISHED' for c in s['calls']), timeout=15)
        path = a.dir / 'early-single.wav'
        reply = a.command('lab_record', f'{call} {path.as_posix()}').strip()
        assert 'started' in reply, 'recording did not start at once: ' + reply
        time.sleep(2)
        a.command('lab_stop')
        with wave.open(str(path), 'rb') as wav:
            assert wav.getnframes() > 0, 'the recording holds no audio'
        print('PASS: 応答後に始めた録音は即座に音を取る', flush=True)
        a.action('hangup', call)
        a.wait(lambda s: not s['calls'])
    finally:
        if a:
            a.close()

    # As the app does it: the first call is being recorded, a second call to
    # the early-media number holds it, and the recording input is released
    # while the second call rings (the app selects nothing when no call is
    # established and unheld). The ringback must not reach the file, and the
    # answered second call must take the recording over.
    a = b = None
    try:
        a = Phone('early-a', configured[0], 18590, 19290)
        b = Phone('early-b', configured[1], 18592, 19300)
        first, remote = connect(a, b, configured[1]['extension'])
        path = a.dir / 'early-switch.wav'
        a.command('lab_record', f'{first} {path.as_posix()}')
        time.sleep(1)
        a.action('hold', first)
        a.command('lab_record_select', '-')
        paused = path.stat().st_size
        second = a.action('dial', value=number)
        a.wait(lambda s: any(c['id'] == second and c['state'] == 'EARLY' for c in s['calls']), timeout=15)
        time.sleep(1.5)
        assert path.stat().st_size == paused, 'the ringback leaked into the recording'
        print('PASS: 呼出中のアーリーメディアは録音に混ざらない', flush=True)
        a.wait(lambda s: any(c['id'] == second and c['state'] == 'ESTABLISHED' for c in s['calls']), timeout=15)
        reply = a.command('lab_record_select', second).strip()
        assert 'switched' in reply or 'reserved' in reply, 'the answered call was not selected: ' + reply
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline and path.stat().st_size == paused:
            time.sleep(.1)
        assert path.stat().st_size > paused, 'the answered call did not take the recording over'
        print('PASS: 応答後は2本目の通話へ録音が続く', flush=True)
        a.command('lab_stop')
        for call in (second, first):
            try:
                a.action('hangup', call)
            except Exception:
                pass
        a.wait(lambda s: not s['calls'])
        b.wait(lambda s: not s['calls'])
    finally:
        if a:
            a.close()
        if b:
            b.close()
    print('PASS: early media neither leaks into nor breaks the recording', flush=True)


if __name__ == '__main__':
    main()
