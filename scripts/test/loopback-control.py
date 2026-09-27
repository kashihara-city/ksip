"""The engine listens for no control: it connects out to the listener its starter holds, names the start's secret first, takes commands only on that connection, and quits when it closes or cannot be made."""
import os, subprocess, sys, time
from pathlib import Path

import engine_control

ROOT = Path(__file__).resolve().parents[2]
BASE = ROOT / 'temp/build/ksip-control-test'
ENGINE = ROOT / 'temp/build/native/bin/baresip.exe'


def start(name, port, secret):
    """An isolated engine with no account, told to connect to `port`."""
    profile = BASE / name
    profile.mkdir(parents=True, exist_ok=True)
    (profile / 'config').write_text(f'sip_listen 127.0.0.1:0\nksip_ctrl_connect 127.0.0.1:{port}\nmodule ksip_ctrl.dll\nmodule menu.dll\n', encoding='utf-8')
    (profile / 'accounts').write_text('', encoding='utf-8')
    env = dict(os.environ)
    env.pop('KSIP_CONTROL_SECRET', None)
    if secret is not None:
        env['KSIP_CONTROL_SECRET'] = secret
    log = (profile / 'engine.log').open('wb')
    process = subprocess.Popen([str(ENGINE), '-f', str(profile)], env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW)
    return process, log, profile / 'engine.log'


def frame(sock, payload):
    import json
    data = json.dumps(payload).encode()
    sock.sendall(str(len(data)).encode() + b':' + data + b',')


def stop(process):
    if process.poll() is None:
        process.kill()
        process.wait()


def main():
    # The engine's greeting names the secret, and it then takes commands.
    secret = engine_control.new_secret()
    listener, port = engine_control.listen()
    process, log, log_path = start('greeting', port, secret)
    try:
        conn = engine_control.accept(listener, process, secret)
        frame(conn, {'command': 'help', 'token': 'h1'})
        conn.settimeout(3)
        answer = engine_control.read_frame(conn)
        assert answer and answer.get('ok') is True and answer.get('token') == 'h1', answer
        print('PASS: the engine connects to its starter, names the secret, and is answered', flush=True)
        # When the connection ends, so does the engine.
        conn.close()
        code = process.wait(timeout=10)
        assert code == 0, code
        print('PASS: the engine quits when its control connection closes', flush=True)
    finally:
        stop(process)
        log.close()
    assert secret.encode() not in log_path.read_bytes(), 'the secret reached the log'
    print('PASS: the secret is not in the log', flush=True)

    # Nobody to connect to: the engine does not wait around uncontrolled.
    listener, port = engine_control.listen()
    listener.close()
    process, log, _ = start('nobody', port, engine_control.new_secret())
    try:
        process.wait(timeout=15)
        print('PASS: an engine that cannot reach its starter ends', flush=True)
    finally:
        stop(process)
        log.close()

    # Without a secret the control module refuses to load and the engine ends
    # without connecting.
    listener, port = engine_control.listen()
    process, log, _ = start('no-secret', port, None)
    try:
        listener.settimeout(0.2)
        deadline = time.monotonic() + 15
        connected = False
        while process.poll() is None and time.monotonic() < deadline:
            try:
                conn, _ = listener.accept()
                connected = True
                conn.close()
            except OSError:
                pass
        assert process.poll() is not None, 'an engine without a secret kept running'
        assert not connected, 'an engine without a secret connected'
        print('PASS: an engine without a secret neither connects nor runs', flush=True)
    finally:
        listener.close()
        stop(process)
        log.close()
    print('PASS: control connection')


if __name__ == '__main__':
    try:
        main()
    except Exception as e:
        print('FAIL:', e)
        sys.exit(1)
