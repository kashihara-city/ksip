"""Transfer between two calls to a PBX faked on 127.0.0.1 and check that only a NOTIFY the engine takes as the REFER's (answered 2xx) ends the REFER's limit: a stale one (answered 481) leaves it running to TRANSFER_UNKNOWN, a right one keeps the transfer waiting until its final NOTIFY."""
import re, socket, threading, time, uuid
from sip_fixture import Phone

# The REFER's limit is 64*T1 + 2 s from sending it (transfer.cpp).
LIMIT = 34


class FakePbx:
    """A UDP SIP peer that registers the phone, answers its calls and holds,
    accepts a REFER and sends the NOTIFY the scenario asks for."""

    def __init__(self, stale):
        self.stale = stale
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(('127.0.0.1', 0))
        self.sock.settimeout(0.2)
        self.port = self.sock.getsockname()[1]
        self.media = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.media.bind(('127.0.0.1', 0))
        self.dialogs = {}
        self.cseq = 100
        self.notify_answers = []
        self.refer = None
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def close(self):
        self.stop.set()
        self.thread.join(2)
        self.sock.close()
        self.media.close()

    def run(self):
        while not self.stop.is_set():
            try:
                data, peer = self.sock.recvfrom(65535)
            except socket.timeout:
                continue
            except OSError:
                return
            text = data.decode('utf-8', errors='replace')
            if not text.strip():
                continue
            head, _, body = text.partition('\r\n\r\n')
            lines = head.split('\r\n')
            headers = {}
            for line in lines[1:]:
                if ':' in line:
                    key, value = line.split(':', 1)
                    headers.setdefault(key.strip().lower(), value.strip())
            if lines[0].startswith('SIP/2.0'):
                if headers.get('cseq', '').endswith('NOTIFY'):
                    self.notify_answers.append(int(lines[0].split()[1]))
                continue
            self.request(lines[0].split()[0], headers, body, peer)

    def reply(self, headers, peer, status, extra='', body='', to_tag=None):
        to = headers['to'] + (f';tag={to_tag}' if to_tag and ';tag=' not in headers['to'] else '')
        content = (f'Content-Type: application/sdp\r\n' if body else '')
        message = (f'SIP/2.0 {status}\r\nVia: {headers["via"]}\r\nFrom: {headers["from"]}\r\nTo: {to}\r\n'
                   f'Call-ID: {headers["call-id"]}\r\nCSeq: {headers["cseq"]}\r\n{extra}{content}'
                   f'Content-Length: {len(body.encode())}\r\n\r\n{body}')
        self.sock.sendto(message.encode(), peer)

    def answer_sdp(self, offer):
        hold = 'a=sendonly' in offer or 'a=inactive' in offer
        return ('v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n'
                f'm=audio {self.media.getsockname()[1]} RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n'
                f'a={"recvonly" if hold else "sendrecv"}\r\n')

    def request(self, method, headers, body, peer):
        contact = f'Contact: <sip:pbx@127.0.0.1:{self.port}>\r\n'
        if method == 'REGISTER':
            self.reply(headers, peer, '200 OK', f'Contact: {headers["contact"]};expires=300\r\nExpires: 300\r\n', to_tag='reg')
        elif method == 'INVITE':
            call_id = headers['call-id']
            dialog = self.dialogs.setdefault(call_id, {'tag': uuid.uuid4().hex[:8], 'from': headers['from'],
                                                       'to': headers['to'], 'contact': headers['contact'], 'peer': peer})
            self.reply(headers, peer, '200 OK', contact, self.answer_sdp(body), to_tag=dialog['tag'])
        elif method == 'REFER':
            self.refer = (headers['call-id'], int(headers['cseq'].split()[0]))
            self.reply(headers, peer, '202 Accepted', contact, to_tag=self.dialogs[headers['call-id']]['tag'])
            threading.Timer(0.5, self.notify, args=('SIP/2.0 100 Trying', 'active;expires=60')).start()
        elif method in ('BYE', 'OPTIONS', 'NOTIFY', 'SUBSCRIBE', 'UPDATE', 'INFO', 'MESSAGE'):
            self.reply(headers, peer, '200 OK', to_tag=self.dialogs.get(headers['call-id'], {}).get('tag'))

    def notify(self, sipfrag, state, stale=None):
        """A NOTIFY in the REFER's dialog: from the PBX's side of it, with the
        Event id of the REFER's CSeq, or one before it when stale."""
        call_id, refer_cseq = self.refer
        dialog = self.dialogs[call_id]
        stale = self.stale if stale is None else stale
        self.cseq += 1
        target = re.search(r'<([^>]+)>', dialog['contact']).group(1)
        body = sipfrag + '\r\n'
        message = (f'NOTIFY {target} SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{self.port};branch=z9hG4bK{uuid.uuid4().hex}\r\n'
                   f'From: {dialog["to"]};tag={dialog["tag"]}\r\nTo: {dialog["from"]}\r\nCall-ID: {call_id}\r\n'
                   f'CSeq: {self.cseq} NOTIFY\r\nContact: <sip:pbx@127.0.0.1:{self.port}>\r\nMax-Forwards: 70\r\n'
                   f'Event: refer;id={refer_cseq - 1 if stale else refer_cseq}\r\nSubscription-State: {state}\r\n'
                   f'Content-Type: message/sipfrag;version=2.0\r\nContent-Length: {len(body.encode())}\r\n\r\n{body}')
        self.sock.sendto(message.encode(), dialog['peer'])


def scenario(name, stale):
    pbx = FakePbx(stale)
    phone = None
    try:
        account = dict(server='127.0.0.1', port=pbx.port, extension='1001', auth_user='1001', password='loopback-test-only')
        phone = Phone(name, account, extra_config='net_interface 127.0.0.1')
        established = lambda call: phone.wait(lambda s: any(c['id'] == call and c['state'] == 'ESTABLISHED' for c in s['calls']), timeout=15)
        first = phone.action('dial', value='2001')
        established(first)
        second = phone.action('dial', value='2002')
        established(second)
        time.sleep(1)
        phone.action('transfer', second, first)
        phone.wait(lambda s: pbx.refer is not None, timeout=10)
        phone.wait(lambda s: pbx.notify_answers, timeout=10)
        answer = pbx.notify_answers[0]
        # Past the REFER's limit, with a second to spare.
        deadline = time.monotonic() + LIMIT + 4
        while time.monotonic() < deadline and phone.state()['transfer']['pending']:
            time.sleep(0.5)
        transfer = phone.state()['transfer']
        result = dict(notify_answer=answer, pending=transfer['pending'], outcome=transfer['outcome'])
        if not stale and transfer['pending']:
            # The final NOTIFY: the transfer is through, and baresip closes the call it went to.
            pbx.notify('SIP/2.0 200 OK', 'terminated;reason=noresource')
            done = phone.wait(lambda s: s['transfer']['outcome'] == 'TRANSFER_DONE', timeout=10)
            result['final'] = done['transfer']['outcome']
        return result
    finally:
        if phone:
            phone.close()
        pbx.close()


def main():
    stale = scenario('transfer-notify-stale', stale=True)
    assert stale['notify_answer'] == 481 and not stale['pending'] and stale['outcome'] == 'TRANSFER_UNKNOWN', f'stale: {stale}'
    print(f"PASS: 古い Event id の NOTIFY は 481 で断られ、転送の期限は残って {LIMIT} 秒で TRANSFER_UNKNOWN になる", flush=True)
    right = scenario('transfer-notify-right', stale=False)
    assert right['notify_answer'] == 200 and right['pending'] and right['outcome'] == 'TRANSFER_PENDING' and right.get('final') == 'TRANSFER_DONE', f'right: {right}'
    print(f"PASS: REFER の NOTIFY が 200 で受け付けられると期限は外れ、{LIMIT} 秒を過ぎても待ち、最後の NOTIFY で TRANSFER_DONE になる", flush=True)


if __name__ == '__main__':
    main()
