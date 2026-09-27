"""The engine's state report says how many certificates are in the trust store it verifies a TLS server with, as loaded from sip_cafile: none from a file that is not certificates or not there, some from a real one, and nothing at all without TLS; the app says an empty store is unusable from this, not from baresip's warning."""
import json, os, ssl, subprocess, time
from pathlib import Path

import engine_control

ROOT = Path(__file__).resolve().parents[2]
BASE = ROOT / 'temp/build/ksip-trust-test'
ENGINE = ROOT / 'temp/build/native/bin/baresip.exe'


def state(name, transport, cafile=None):
    """The engine's ksip_state, from an isolated engine with no account."""
    profile = BASE / name
    profile.mkdir(parents=True, exist_ok=True)
    listener, port = engine_control.listen()
    trust = f'sip_cafile {Path(cafile).as_posix()}\nsip_verify_server yes\n' if cafile else ''
    (profile / 'config').write_text(f'sip_listen 0.0.0.0:0\nnet_interface 127.0.0.1\nsip_transports {transport}\n{trust}'
                                    f'ksip_ctrl_connect 127.0.0.1:{port}\nmodule ksip_ctrl.dll\nmodule menu.dll\nmodule ksip.dll\n', encoding='utf-8')
    (profile / 'accounts').write_text('', encoding='utf-8')
    secret = engine_control.new_secret()
    log = (profile / 'engine.log').open('wb')
    process = subprocess.Popen([str(ENGINE), '-f', str(profile)], cwd=ENGINE.parent, env=dict(os.environ, KSIP_CONTROL_SECRET=secret),
                               stdin=subprocess.DEVNULL, stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW)
    try:
        try:
            conn = engine_control.accept(listener, process, secret)
        finally:
            listener.close()
        data = json.dumps({'command': 'ksip_state', 'params': '', 'token': 's1'}).encode()
        conn.sendall(str(len(data)).encode() + b':' + data + b',')
        deadline = time.monotonic() + 8
        while True:
            answer = engine_control.read_frame(conn, deadline)
            assert answer is not None, 'the engine closed the connection before answering'
            if answer.get('token') == 's1':
                assert answer.get('ok'), answer
                return json.loads(answer['data'])
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        log.close()


def main():
    BASE.mkdir(parents=True, exist_ok=True)
    broken = BASE / 'not-certificates.pem'
    broken.write_text('this is not a certificate\n', encoding='utf-8')
    missing = BASE / 'no-such-file.pem'
    missing.unlink(missing_ok=True)
    # A real trust list: what Windows trusts, as Python's ssl reads it.
    context = ssl.create_default_context()
    real = BASE / 'windows-roots.pem'
    real.write_text(''.join(ssl.DER_cert_to_PEM_cert(der) for der in context.get_ca_certs(binary_form=True)), encoding='ascii')
    failures = 0

    def check(ok, what, detail):
        nonlocal failures
        print(('PASS: ' if ok else 'FAIL: ') + what + ('' if ok else f' ({detail})'), flush=True)
        failures += not ok

    s = state('broken', 'tls', broken)
    check(s.get('tls_trust_certificates') == 0, 'a CA file that is not certificates leaves the store empty, and the report says 0', s.get('tls_trust_certificates'))
    s = state('missing', 'tls', missing)
    check(s.get('tls_trust_certificates') == 0, 'a CA file that is not there leaves the store empty, and the report says 0', s.get('tls_trust_certificates'))
    s = state('real', 'tls', real)
    check((s.get('tls_trust_certificates') or 0) > 0, 'a real CA file fills the store, and the report counts it', s.get('tls_trust_certificates'))
    s = state('udp', 'udp')
    check('tls_trust_certificates' not in s, 'without TLS there is no store and nothing is said', s.get('tls_trust_certificates'))
    check('audio' in s and s['audio']['ready'] is False, 'an engine without the audio module says its audio is not up', s.get('audio'))
    print(f"{'FAIL' if failures else 'PASS'}: {failures} failure(s)")
    raise SystemExit(1 if failures else 0)


if __name__ == '__main__':
    main()
