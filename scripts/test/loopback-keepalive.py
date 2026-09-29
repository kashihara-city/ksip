"""Register the built engine with a registrar faked on 127.0.0.1 and check where its keepalives go: to the registrar after its 200 OK (folded headers included), never to a stranger that sends a 200 OK of its own."""
import socket, threading, time
from sip_fixture import Phone

INTERVAL = 10


def scenario(name, fold=False, stranger_answers=False):
    """Registers once against a fake registrar and waits past one keepalive
    interval. Returns (registration, transport, keepalives the registrar got,
    keepalives the stranger got)."""
    stop = threading.Event()
    registrar = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    registrar.bind(('127.0.0.1', 0))
    registrar.settimeout(0.2)
    stranger = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    stranger.bind(('127.0.0.1', 0))
    stranger.settimeout(0.2)
    answers, keepalives = [], []

    def answer():
        while not stop.is_set():
            try:
                data, peer = registrar.recvfrom(65535)
            except socket.timeout:
                continue
            if data == b'\r\n\r\n':
                keepalives.append(time.monotonic())
                continue
            text = data.decode('utf-8', errors='replace')
            if not text.startswith('REGISTER '):
                continue
            headers = {}
            for line in text.split('\r\n')[1:]:
                if ':' in line:
                    key, value = line.split(':', 1)
                    headers.setdefault(key.strip().lower(), value.strip())
            via, cseq = headers['via'], headers['cseq']
            if fold:
                # Folded as RFC 3261 7.3.1 allows: the branch, and the CSeq's method, on lines of their own.
                via = via.replace(';branch=', ';\r\n branch=')
                cseq = cseq.replace(' ', '\r\n\t', 1)
            reply = ('SIP/2.0 200 OK\r\nVia: ' + via + '\r\nFrom: ' + headers['from'] + '\r\nTo: ' + headers['to'] +
                     ';tag=loopback\r\nCall-ID: ' + headers['call-id'] + '\r\nCSeq: ' + cseq + '\r\nContact: ' +
                     headers['contact'] + ';expires=300\r\nExpires: 300\r\nContent-Length: 0\r\n\r\n').encode()
            answers.append((reply, peer))
            registrar.sendto(reply, peer)

    thread = threading.Thread(target=answer, daemon=True)
    thread.start()
    phone = None
    try:
        account = dict(server='127.0.0.1', port=registrar.getsockname()[1], extension='1001', auth_user='1001', password='loopback-test-only')
        phone = Phone(name, account, extra_config=f'net_interface 127.0.0.1\nksip_keepalive crlf\nksip_keepalive_interval {INTERVAL}')
        if stranger_answers:
            # Another's 200 OK to a REGISTER: the same message but for its Call-ID, from another address.
            reply, peer = answers[-1]
            stranger.sendto(reply.replace(b'Call-ID: ', b'Call-ID: stranger-'), peer)
        time.sleep(INTERVAL + 1.5)
        state = phone.state()
        to_stranger = 0
        while True:
            try:
                to_stranger += stranger.recvfrom(65535)[0] == b'\r\n\r\n'
            except socket.timeout:
                break
        return state['registration'], state.get('transport'), len(keepalives), to_stranger
    finally:
        if phone:
            phone.close()
        stop.set()
        thread.join(2)
        registrar.close()
        stranger.close()


def main():
    plain = scenario('keepalive-plain', stranger_answers=True)
    assert plain[:2] == ('REGISTER_OK', 'UDP') and plain[2] >= 1 and plain[3] == 0, f'plain: {plain}'
    print(f'PASS: 登録先の 200 OK の後、空行は登録先へ届き、別の送り主の 200 OK には送られない（{plain[2]}回）')
    folded = scenario('keepalive-folded', fold=True)
    assert folded[:2] == ('REGISTER_OK', 'UDP') and folded[2] >= 1, f'folded: {folded}'
    print(f'PASS: Via と CSeq を折り返した 200 OK でも、空行は登録先へ届く（{folded[2]}回）')


if __name__ == '__main__':
    main()
