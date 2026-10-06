"""Run the test suite, or a group of it, one test after another against the chosen PBX, and write a summary with one log per test."""
import argparse, datetime, os, pathlib, subprocess, sys, time,hashlib

ROOT = pathlib.Path(__file__).resolve().parents[2]
TESTS = ROOT / 'scripts/test'
# The order matters: the cheap checks first, the app last because it takes the
# extension the phones use, and the group is what --group selects. The device
# group needs a real microphone and speaker and takes minutes, so it runs only
# when asked for.
SUITE = [
    ('offline', 'rust.ps1'), ('offline', 'clang-tidy.ps1'), ('offline', 'aec.ps1'), ('offline', 'audio-module.ps1'), ('offline', 'webrtc-restart.ps1'), ('offline', 'audio-devices.py'),
    ('offline', 'supply-chain.py'), ('offline', 'i18n.py'), ('offline', 'settings-dialog.py'), ('offline', 'no-secrets.py'),
    ('offline', 'build-paths.py'), ('offline', 'line-endings.py'), ('offline', 'admx.py'), ('offline', 'sbom.py'),
    ('device', 'webrtc-device.ps1'), ('device', 'pbx-device-switch.py'),
    ('loopback', 'loopback-control.py'), ('loopback', 'loopback-call.py'), ('loopback', 'loopback-gain.py'), ('loopback', 'loopback-silent-mic.py'), ('loopback', 'loopback-trust.py'), ('loopback', 'loopback-keepalive.py'), ('loopback', 'loopback-transfer-notify.py'), ('loopback', 'loopback-subscription-retry.py'), ('loopback', 'loopback-sendonly.py'), ('loopback', 'loopback-rtcp-port.py'),
    ('pbx', 'pbx-transfer.py'), ('pbx', 'pbx-codec.py'), ('pbx', 'pbx-tcp.py'), ('pbx', 'pbx-keepalive.py'), ('pbx', 'pbx-tls.py'), ('pbx', 'pbx-osrtp.py'), ('pbx', 'pbx-pai.py'), ('pbx', 'pbx-blf-54.py'), ('pbx', 'pbx-dtmf.py'),
    ('pbx', 'pbx-record-switch.py'), ('pbx', 'pbx-early-media.py'), ('pbx', 'pbx-hold-media.py'), ('pbx', 'pbx-playback.py'), ('pbx', 'pbx-live-aec.py'),
    ('app', 'app-walkthrough.ps1'), ('app', 'app-protocol.ps1'), ('app', 'app-buttons.ps1'),
    ('app', 'app-transfer.ps1'), ('app', 'app-auto-answer.ps1'), ('app', 'app-unregister.ps1'),
    ('app', 'app-shortcut.ps1'), ('app', 'app-language.ps1'), ('app', 'app-devices.ps1'),
    ('app', 'app-adapter.ps1'), ('app', 'app-aec-calibration.ps1'), ('app', 'app-tls.ps1'),
    ('app', 'app-single-exe.ps1'), ('app', 'app-call-events.ps1'), ('app', 'app-engine-exit.ps1'), ('app', 'app-settings.py'), ('app', 'app-settings-file.ps1'),
]

def stop_test_apps():
    """A test app left behind makes the next UI test time out, and one still
    exiting keeps the exe locked; only apps run from temp/ are stopped."""
    query = 'Get-Process ksip -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "' + str(ROOT / 'temp') + '*" } | Stop-Process -Force -ErrorAction SilentlyContinue'
    subprocess.run(['powershell', '-NoProfile', '-Command', query], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

def run(script, log, folder):
    if script.endswith('.py'):
        command = [sys.executable, '-X', 'utf8', str(TESTS / script)]
        if folder and (script.startswith('app-') or script == 'admx.py'):
            command += ['--folder', str(folder)]
    else:
        command = ['powershell', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', str(TESTS / script)]
        if folder and script.startswith('app-'):
            command += ['-Folder', str(folder)]
    with open(log, 'wb') as out:
        code = subprocess.run(command, cwd=ROOT, stdout=out, stderr=subprocess.STDOUT).returncode
    text = log.read_text(encoding='utf-8', errors='replace')
    skipped = code == 0 and any(line.startswith('SKIP:') for line in text.splitlines())
    return 'SKIP' if skipped else 'PASS' if code == 0 else f'FAIL({code})'

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--group', action='append', choices=['offline', 'device', 'loopback', 'pbx', 'app'], help='which groups to run; default all but device')
    parser.add_argument('--only', action='append', help='a test file name to run, repeatable; overrides --group')
    parser.add_argument('--pbx', help='folder under test-pbx/ to test against (sets KSIP_TEST_PBX)')
    parser.add_argument('--folder', help='a staged build folder for the app tests (their -Folder)')
    parser.add_argument('--gap', type=float, default=3, help='seconds between tests, for the previous app to let go of the exe')
    args = parser.parse_args()
    if args.pbx:
        os.environ['KSIP_TEST_PBX'] = args.pbx
    pbx = os.environ.get('KSIP_TEST_PBX', 'local-asterisk')
    # Windows PowerShell's own module path, or Get-FileHash goes missing when started from pwsh.
    os.environ['PSModulePath'] = ';'.join([os.path.expandvars(r'%USERPROFILE%\Documents\WindowsPowerShell\Modules'),
                                           r'C:\Program Files\WindowsPowerShell\Modules', r'C:\Windows\system32\WindowsPowerShell\v1.0\Modules'])
    chosen = [s for g, s in SUITE if (args.only and s in args.only) or
              (not args.only and (g in args.group if args.group else g != 'device'))]
    stamp = datetime.datetime.now().strftime('%Y%m%d-%H%M%S')
    out = ROOT / 'temp/reports' / f'run-{stamp}-{pbx}'
    out.mkdir(parents=True, exist_ok=True)
    summary = out / 'summary.txt'
    # What was tested: the commit (and whether the tree was clean) and the
    # executable the app tests use, so that a summary can be read back later.
    commit = subprocess.run(['git', 'describe', '--tags', '--always', '--dirty'], cwd=ROOT, capture_output=True, text=True).stdout.strip() or 'unknown'
    exe = pathlib.Path(args.folder) / 'ksip.exe' if args.folder else ROOT / 'release/ksip.exe'
    exe_note = f'{exe} sha256={hashlib.sha256(exe.read_bytes()).hexdigest()[:16]}' if exe.is_file() else f'{exe} (missing)'
    lines = [f'started {datetime.datetime.now():%Y-%m-%dT%H:%M:%S} pbx={pbx} folder={args.folder or "release"}',
             f'source {commit}', f'exe {exe_note}']
    summary.write_text('\n'.join(lines) + '\n', encoding='utf-8')
    failed = 0
    for script in chosen:
        stop_test_apps()
        time.sleep(args.gap)
        started = time.monotonic()
        result = run(script, out / (pathlib.Path(script).stem + '.log'), args.folder)
        line = f'{script}\t{result}\t{int(time.monotonic() - started)}s'
        print(line, flush=True)
        with open(summary, 'a', encoding='utf-8') as f:
            f.write(line + '\n')
        failed += result.startswith('FAIL')
    # A WebRTC update or a change to its patches is proven only on real
    # devices, which is easy to forget when the default leaves them out.
    device_note = [] if any(g == 'device' and s in chosen for g, s in SUITE) else [
        'NOTE: 実機の WebRTC テスト（--group device）は流していません。WebRTC を更新したときや '
        'scripts/build/patch-webrtc.py を変えたときは、マイクとスピーカーのある PC で流してください']
    with open(summary, 'a', encoding='utf-8') as f:
        f.write(''.join(line + '\n' for line in device_note))
        f.write(f'finished {datetime.datetime.now():%Y-%m-%dT%H:%M:%S} failed={failed}\n')
    for line in device_note:
        print(line)
    print(f'{len(chosen)} tests, {failed} failed; logs in {out.relative_to(ROOT).as_posix()}')
    return 1 if failed else 0

if __name__ == '__main__':
    sys.exit(main())
