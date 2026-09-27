"""The engine's control connection takes commands only from the one connection that says the start's secret: an unauthenticated, a wrongly authenticated and a second correct connection are all closed and the first goes on working."""
import json, secrets, socket, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROFILE = ROOT / 'temp/build/ksip-control-test'


def frame(payload):
    data = json.dumps(payload).encode()
    return str(len(data)).encode() + b':' + data + b','


def read_frame(sock, timeout=3.0):
    """One netstring frame, or None when the engine closed the connection."""
    sock.settimeout(timeout)
    header = b''
    while True:
        byte = sock.recv(1)
        if not byte:
            return None
        if byte == b':':
            break
        header += byte
    length = int(header)
    body = b''
    while len(body) < length + 1:
        chunk = sock.recv(length + 1 - len(body))
        if not chunk:
            return None
        body += chunk
    assert body[-1:] == b',', body
    return json.loads(body[:-1])


def closed_by_engine(sock):
    """True when the engine closes the connection within the grace time."""
    try:
        return read_frame(sock, 6.0) is None
    except socket.timeout:
        return False
    except OSError:
        # Refused at the door: the engine resets rather than closes.
        return True


def main():
    PROFILE.mkdir(parents=True, exist_ok=True)
    with socket.socket() as reserve:
        reserve.bind(('127.0.0.1', 0))
        port = reserve.getsockname()[1]
    secret = secrets.token_hex(32)
    (PROFILE / 'config').write_text(f'sip_listen 127.0.0.1:0\nksip_ctrl_listen 127.0.0.1:{port}\nmodule ksip_ctrl.dll\nmodule menu.dll\n', encoding='utf-8')
    (PROFILE / 'accounts').write_text('', encoding='utf-8')
    env = dict(__import__('os').environ, KSIP_CONTROL_SECRET=secret)
    with (PROFILE / 'engine.log').open('wb') as log:
        engine = subprocess.Popen([str(ROOT / 'temp/build/native/bin/baresip.exe'), '-f', str(PROFILE)], env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW)
        sockets = []
        try:
            deadline = time.monotonic() + 10
            while True:
                try:
                    first = socket.create_connection(('127.0.0.1', port), .5)
                    break
                except OSError:
                    if time.monotonic() >= deadline or engine.poll() is not None:
                        raise RuntimeError('the isolated engine did not start; see ' + str(PROFILE / 'engine.log'))
                    time.sleep(.05)
            sockets.append(first)
            # The app's connection: the secret first, then a command.
            first.sendall(frame({'command': 'auth', 'params': secret, 'token': 'auth'}))
            answer = read_frame(first)
            assert answer and answer.get('ok') is True and answer.get('token') == 'auth', answer
            first.sendall(frame({'command': 'help', 'token': 'h1'}))
            answer = read_frame(first)
            assert answer and answer.get('ok') is True, answer
            print('PASS: the connection that says the secret is answered', flush=True)

            # Nobody else: a second connection is refused at the door while the first holds.
            intruder = socket.create_connection(('127.0.0.1', port), 2)
            sockets.append(intruder)
            intruder.sendall(frame({'command': 'quit', 'token': 'q'}))
            assert closed_by_engine(intruder), 'an unauthenticated connection was not closed'
            time.sleep(.5)
            assert engine.poll() is None, 'an unauthenticated quit ended the engine'
            print('PASS: an unauthenticated quit is refused and the engine goes on', flush=True)

            rival = socket.create_connection(('127.0.0.1', port), 2)
            sockets.append(rival)
            rival.sendall(frame({'command': 'auth', 'params': secret, 'token': 'auth'}))
            assert closed_by_engine(rival), 'a second connection with the right secret took over'
            first.sendall(frame({'command': 'help', 'token': 'h2'}))
            answer = read_frame(first)
            assert answer and answer.get('ok') is True and answer.get('token') == 'h2', answer
            print('PASS: even the right secret does not push the first connection off', flush=True)

            # Without the first: a wrong secret is closed, then the right one is taken.
            first.close()
            time.sleep(.3)
            wrong = socket.create_connection(('127.0.0.1', port), 2)
            sockets.append(wrong)
            wrong.sendall(frame({'command': 'auth', 'params': secret[::-1], 'token': 'auth'}))
            assert closed_by_engine(wrong), 'a wrong secret was not closed'
            silent = socket.create_connection(('127.0.0.1', port), 2)
            sockets.append(silent)
            assert closed_by_engine(silent), 'a connection that says nothing was not closed in time'
            print('PASS: a wrong secret and a silent connection are closed', flush=True)

            second = socket.create_connection(('127.0.0.1', port), 2)
            sockets.append(second)
            second.sendall(frame({'command': 'auth', 'params': secret, 'token': 'auth'}))
            answer = read_frame(second)
            assert answer and answer.get('ok') is True, answer
            second.sendall(frame({'command': 'quit', 'token': 'bye'}))
            code = engine.wait(timeout=10)
            assert code == 0, code
            print('PASS: the authenticated connection can end the engine', flush=True)
            log_text = (PROFILE / 'engine.log').read_bytes()
            assert secret.encode() not in log_text, 'the secret reached the log'
            print('PASS: the secret is not in the log', flush=True)
        finally:
            for sock in sockets:
                try:
                    sock.close()
                except OSError:
                    pass
            if engine.poll() is None:
                engine.kill()
                engine.wait()
    print('PASS: control connection authentication')


if __name__ == '__main__':
    try:
        main()
    except Exception as e:
        print('FAIL:', e)
        sys.exit(1)
