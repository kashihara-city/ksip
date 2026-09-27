"""The test side of the engine's control connection: a listener the engine connects to, and the engine's greeting checked before anything is said."""
import json, secrets, socket, time


def new_secret():
    """A secret for one engine start, as the app makes one."""
    return secrets.token_hex(32)


def listen(port=0):
    """The listener the engine connects to, held until it has: on 127.0.0.1,
    on the given port or a free one. Returns (listener, port)."""
    listener = socket.socket()
    listener.bind(('127.0.0.1', port))
    listener.listen(4)
    return listener, listener.getsockname()[1]


def read_frame(sock, until=None):
    """One netstring frame as JSON, or None when the peer closed first. With
    `until` (a time.monotonic() deadline) the whole frame has to arrive by then,
    however slowly it is sent."""
    def recv(n):
        if until is not None:
            left = until - time.monotonic()
            if left <= 0:
                raise socket.timeout('the frame took too long')
            sock.settimeout(left)
        return sock.recv(n)
    header = b''
    while True:
        byte = recv(1)
        if not byte:
            return None
        if byte == b':':
            break
        header += byte
        if len(header) > 7 or not byte.isdigit():
            raise ValueError('not a netstring frame')
    length = int(header)
    body = b''
    while len(body) < length + 1:
        chunk = recv(length + 1 - len(body))
        if not chunk:
            return None
        body += chunk
    if body[-1:] != b',':
        raise ValueError('not a netstring frame')
    return json.loads(body[:-1])


def accept(listener, process, secret, timeout=10):
    """Waits for the engine to connect and name the secret, as the app does:
    nothing is said to a connection before its greeting, and any other
    connection is closed. The listener is closed once the engine is taken."""
    listener.settimeout(0.2)
    deadline = time.monotonic() + timeout
    while True:
        # The overall deadline holds however many strangers come first.
        if process.poll() is not None:
            raise RuntimeError('the engine ended before it connected')
        if time.monotonic() > deadline:
            raise RuntimeError('the engine did not connect')
        try:
            conn, _ = listener.accept()
        except socket.timeout:
            continue
        # Each connection has two seconds for its whole greeting, and never
        # past the overall deadline.
        try:
            hello = read_frame(conn, min(deadline, time.monotonic() + 2))
        except (OSError, ValueError):
            hello = None
        if isinstance(hello, dict) and hello.get('hello') == 'ksip_ctrl' and hello.get('secret') == secret:
            conn.settimeout(None)
            listener.close()
            return conn
        conn.close()
