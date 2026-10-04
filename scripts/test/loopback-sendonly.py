"""A peer that only sends (a=sendonly: hold music, an announcement) gets empty datagrams from this side's RTP and RTCP sockets at once and again after twenty seconds of nothing sent, so that what it sends gets through a Windows Firewall without a rule, whether it answered the call (200) or only sends early media (183); a peer that sends and receives gets RTP as before. A fake PBX on 127.0.0.1 answers the INVITE and counts what arrives at its media ports."""
from pathlib import Path
import json, socket, sys, threading, time
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/test'))
from sip_fixture import Phone


class FakePbx:
    """A registrar and one extension that answers every INVITE with the given direction: with 200 OK, or with 183 Session Progress and never more (`early`). The INVITEs are kept, for their SDP."""

    def __init__(self, direction, early=False):
        self.direction = direction
        self.early = early
        self.invites = []
        self.sip = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sip.bind(('127.0.0.1', 0)); self.sip.settimeout(0.1)
        self.media = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.media.bind(('127.0.0.1', 0)); self.media.settimeout(0.05)
        self.rtcp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.rtcp.bind(('127.0.0.1', 0)); self.rtcp.settimeout(0.05)
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    @property
    def port(self):
        return self.sip.getsockname()[1]

    def serve(self):
        while not self.stop.is_set():
            try:
                data, peer = self.sip.recvfrom(65535)
            except socket.timeout:
                continue
            text = data.decode('utf-8', 'replace')
            method = text.split(' ', 1)[0]
            headers = {}
            for line in text.split('\r\n')[1:]:
                if ':' in line:
                    name, value = line.split(':', 1)
                    headers.setdefault(name.lower(), value.strip())
            if method not in ('REGISTER', 'INVITE', 'BYE', 'ACK'):
                continue
            if method == 'ACK':
                continue
            body = ''
            status = '200 OK'
            if method == 'INVITE':
                self.invites.append(text)
                body = (f'v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=fake\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n'
                        f'm=audio {self.media.getsockname()[1]} RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\na={self.direction}\r\n'
                        f'a=rtcp:{self.rtcp.getsockname()[1]} IN IP4 127.0.0.1\r\n')
                if self.early:
                    status = '183 Session Progress'
            reply = f'SIP/2.0 {status}\r\n' + ''.join(f'{name}: {headers[name.lower()]}\r\n' for name in ('Via', 'From', 'Call-ID', 'CSeq'))
            reply += f'To: {headers["to"]};tag=fake\r\n'
            if method == 'REGISTER':
                reply += f'Contact: {headers["contact"]};expires=300\r\nExpires: 300\r\n'
            else:
                reply += f'Contact: <sip:1002@127.0.0.1:{self.port}>\r\n'
            if body:
                reply += 'Content-Type: application/sdp\r\n'
            self.sip.sendto((reply + f'Content-Length: {len(body)}\r\n\r\n' + body).encode(), peer)

    def count(self, seconds):
        """What arrived at the media ports in the time: RTP packets, empty datagrams, RTCP packets, and the ports the RTCP came from."""
        counts = {'rtp': 0, 'empty': 0, 'rtcp': 0, 'rtcp_from': []}
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            try:
                packet, _ = self.media.recvfrom(65535)
                counts['rtp' if packet else 'empty'] += 1
            except socket.timeout:
                pass
            try:
                _, peer = self.rtcp.recvfrom(65535)
                counts['rtcp'] += 1
                if peer[1] not in counts['rtcp_from']:
                    counts['rtcp_from'].append(peer[1])
            except socket.timeout:
                pass
        return counts

    def close(self):
        self.stop.set()
        self.thread.join(timeout=2)
        for s in (self.sip, self.media, self.rtcp):
            s.close()


def scenario(direction, early=False):
    pbx = FakePbx(direction, early)
    account = dict(server='127.0.0.1', port=pbx.port, extension='1001', auth_user='1001', password='fake-only')
    phone = None
    result = {'direction': direction, 'early': early}
    try:
        phone = Phone('sendonly-' + direction + ('-early' if early else ''), account, sip_port=0, rtp_port=0, codecs=('g711',),
                      extra_config='net_interface 127.0.0.1\nfilter_registrar UDP,TCP,TLS')
        call = phone.action('dial', value='1002')
        state = 'EARLY' if early else 'ESTABLISHED'
        phone.wait(lambda s: any(c['id'] == call and c['state'] == state for c in s['calls']))
        result['first_two_seconds'] = pbx.count(2)
        if direction == 'sendonly':
            # The way in opened again after twenty seconds without RTP: the
            # engine looks every five, so within thirty from the first.
            result['next_thirty_seconds'] = pbx.count(30)
        result['log'] = [l.strip()[:160] for l in phone.log_text().splitlines() if 'keep the way in open' in l]
        phone.action('hangup', call)
        phone.wait(lambda s: not s['calls'], timeout=10)
    finally:
        if phone:
            phone.close()
        pbx.close()
    return result


def main():
    (ROOT / 'temp/reports').mkdir(parents=True, exist_ok=True)
    only_sends = scenario('sendonly')
    early = scenario('sendonly', early=True)
    both = scenario('sendrecv')
    (ROOT / 'temp/reports/loopback-sendonly.json').write_text(json.dumps({'sendonly': only_sends, 'early': early, 'sendrecv': both}, ensure_ascii=False, indent=2), encoding='utf-8')
    for name, result in (('answered', only_sends), ('early media', early)):
        first = result['first_two_seconds']
        assert first['rtp'] == 0, f'a peer that only sends ({name}) got RTP: {first}'
        assert first['empty'] >= 1, f'no empty datagram reached the peer that only sends ({name}) within two seconds: {first}'
        assert any('the peer only sends' in l for l in result['log']), (name, result['log'])
        later = result['next_thirty_seconds']
        assert later['empty'] >= 1, f'no empty datagram within the thirty seconds after ({name}): {later}'
        assert any('no RTP sent for 20 s' in l for l in result['log']), (name, result['log'])
    assert both['first_two_seconds']['rtp'] > 0, f'a peer that sends and receives got no RTP: {both}'
    first, later = only_sends['first_two_seconds'], only_sends['next_thirty_seconds']
    print(f"PASS: a=sendonly: no RTP, {first['empty']} empty datagram(s) at once and {later['empty']} more within thirty seconds, "
          f"the same for early media ({early['first_two_seconds']['empty']} and {early['next_thirty_seconds']['empty']}); "
          f"a=sendrecv: {both['first_two_seconds']['rtp']} RTP packets in two seconds")


if __name__ == '__main__':
    main()
