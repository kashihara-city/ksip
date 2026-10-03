"""Switch the microphone and the speaker during a call to the lab PBX's playback number, through the engine's ksip_audio_devices as the app does, and check that the call goes on through the new devices: the playback reaches the new speaker, the microphone is the device, and the call stays up. Needs two speakers and two microphones on this machine."""
import json, subprocess, time
from sip_fixture import ROOT, Phone, accounts, numbers

HELPER = ROOT / 'temp/cargo-target/debug/examples/audio-devices.exe'


def peak(device):
    result = subprocess.run([str(HELPER), 'peak', 'speaker', device], capture_output=True, encoding="utf-8")
    try:
        return json.loads(result.stdout)['peak']
    except (ValueError, KeyError):
        return 0.0


def loudest(device, seconds=4):
    top, end = 0.0, time.monotonic() + seconds
    while time.monotonic() < end:
        top = max(top, peak(device))
        time.sleep(0.05)
    return top


def main():
    assert HELPER.is_file(), 'scripts/test/rust.ps1 を先に流す（デバイスの一覧を読む補助がまだ無い）'
    devices = json.loads(subprocess.run([str(HELPER)], capture_output=True, encoding="utf-8", check=True).stdout)
    microphones = [d['id'] for d in devices if d['kind'] == 'microphone']
    speakers = [d['id'] for d in devices if d['kind'] == 'speaker']
    if len(microphones) < 2 or len(speakers) < 2:
        print(f'SKIP: マイクとスピーカーが2つずつ要る（マイク {len(microphones)}、スピーカー {len(speakers)}）', flush=True)
        return
    phone = Phone('device-switch', accounts()[0], audio_player=f'ksip_audio,{speakers[0]}', audio_source=f'ksip_audio,{microphones[0]}',
                  extra_config='module ksip_audio.dll\nksip_aec_enabled yes\nksip_microphone_gain 100\nksip_speaker_gain 100')
    try:
        call = phone.action('dial', value=numbers()['playback'])
        phone.wait(lambda s: any(c['id'] == call and c['state'] == 'ESTABLISHED' for c in s['calls']), timeout=20)
        time.sleep(2)
        for microphone, speaker in ((microphones[1], speakers[1]), (microphones[0], speakers[0])):
            before = phone.log_text().count("the call's speaker switched")
            phone.command('ksip_audio_devices', f'{microphone},{speaker}')
            heard = loudest(speaker)
            state = phone.state()
            log = phone.log_text()
            assert log.count("the call's speaker switched") == before + 1 and "the call's microphone switched" in log, log[-2000:]
            assert any(c['id'] == call and c['state'] == 'ESTABLISHED' for c in state['calls']), state['calls']
            assert state['audio']['microphone']['input'] == 'device' and state['audio']['speaker']['playing'], state['audio']
            assert heard > 0.01, f'the playback did not reach {speaker} (peak {heard})'
            print(f'PASS: 通話中に切り替えると、通話は切れずに新しいデバイスへ移り、再生番号の音が {speaker} に届く（最大 {heard:.3f}）', flush=True)
        phone.action('hangup', call)
        phone.wait(lambda s: not s['calls'], timeout=10)
    finally:
        phone.close()


if __name__ == '__main__':
    main()
