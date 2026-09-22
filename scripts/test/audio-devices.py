"""Real Windows endpoint checks; no SIP or audio streams. Restore changed volume."""
from pathlib import Path
import json, os, subprocess

ROOT = Path(__file__).resolve().parents[2]
EXE = Path(os.environ.get('KSIP_AUDIO_TEST_EXE', str(ROOT / 'temp/cargo-target/debug/examples/audio-devices.exe')))

def command(*args, success=True):
    p = subprocess.run([str(EXE), *map(str, args)], capture_output=True,
                       encoding='utf-8', timeout=8, creationflags=subprocess.CREATE_NO_WINDOW)
    if not success:
        assert p.returncode != 0, args
        return
    assert p.returncode == 0, p.stderr
    return json.loads(p.stdout)

def main():
    (ROOT / 'temp/reports').mkdir(parents=True, exist_ok=True)
    devices = command()
    assert len({d['id'] for d in devices}) == len(devices)
    results = []
    for kind in ('microphone', 'speaker'):
        original = command('volume', kind, 'default')
        assert any(d['id'] == original['id'] and d['kind'] == kind for d in devices)
        device = original['id']
        assert command('volume', kind, device) == original
        peak = command('peak', kind, device)
        assert peak['id'] == device and 0.0 <= peak['peak'] <= 1.0
        command('volume', kind, device, 101, success=False)
        command('volume', kind, device, -1, success=False)
        other = 'speaker' if kind == 'microphone' else 'microphone'
        command('volume', other, device, original['level'], success=False)
        target = original['level'] - 1 if original['level'] else 1
        try:
            command('volume', kind, device, target)
            changed = command('volume', kind, device)
            assert abs(changed['level'] - target) <= 1
            assert changed['muted'] == original['muted']
        finally:
            command('volume', kind, device, original['level'])
        restored = command('volume', kind, device)
        assert restored == original
        results.append({'kind': kind, 'original': original, 'peak': peak, 'changed': changed, 'restored': restored})
    command('volume', 'speaker', 'missing-endpoint', success=False)
    report = {'implementation': 'Rust Core Audio', 'devices': devices, 'volumeReadWriteRestore': results, 'invalidArgumentsRejected': True}
    (ROOT / 'temp/reports/audio-devices-test.json').write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding='utf-8')
    print('PASS: enumeration, default/explicit endpoints, peak, volume read/write/restore, invalid arguments')

if __name__ == '__main__':
    main()
