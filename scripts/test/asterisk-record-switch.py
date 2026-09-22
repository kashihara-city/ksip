"""Switching the recording to a call that has no audio yet must be reserved, not refused."""
import time
import wave

from sip_fixture import Phone, account, accounts, connect


def main():
    recorder = callee = None
    try:
        third = account(2, '録音切替の3台目')
        recorder = Phone('switch-a', accounts()[1], 18586, 19230)
        callee = Phone('switch-c', third, 18588, 19260)
        first, remote = connect(recorder, callee, third['extension'])
        path = recorder.dir / 'switch.wav'
        recorder.command('lab_record', first + ' ' + path.as_posix())
        time.sleep(1)

        # The second call rings without carrying audio, which is where the old build
        # answered EAGAIN and left the recording detached.
        second = recorder.action('dial', value=third['extension'])
        recorder.wait(lambda s: any(c['id'] == second and c['state'] in ('RINGING', 'EARLY')
                                    for c in s['calls']))
        reply = recorder.command('lab_record_select', second).strip()
        assert 'reserved' in reply, 'the ringing call was not reserved: ' + reply
        print('PASS: 音声が始まっていない通話への切替が予約された', flush=True)

        reserved = path.stat().st_size
        time.sleep(1.5)
        assert path.stat().st_size == reserved, 'audio was recorded while no call was bound'
        print('PASS: 予約中は録音されない', flush=True)

        ringing = callee.wait(lambda s: any(c['state'] == 'INCOMING' and c['id'] != remote
                                            for c in s['calls']))
        second_remote = next(c['id'] for c in ringing['calls']
                             if c['state'] == 'INCOMING' and c['id'] != remote)
        callee.action('answer', second_remote)
        recorder.wait(lambda s: any(c['id'] == second and c['state'] == 'ESTABLISHED'
                                    for c in s['calls']))
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline and path.stat().st_size == reserved:
            time.sleep(.1)
        assert path.stat().st_size > reserved, 'the reserved call never took over the recording'
        print('PASS: 応答後に予約された通話へ録音が引き継がれた', flush=True)

        recorder.command('lab_stop')
        with wave.open(str(path), 'rb') as wav:
            assert wav.getnframes() > 0, 'the recording holds no audio'
        for call in (second, first):
            try:
                recorder.action('hangup', call)
            except Exception:
                pass
        print('PASS: recording switch reserves a call until its audio starts')
    finally:
        if recorder:
            recorder.close()
        if callee:
            callee.close()


if __name__ == '__main__':
    main()
