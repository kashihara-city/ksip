"""Watch numbers at a PBX faked on 127.0.0.1 that ends or refuses their subscriptions in every way the engine tells apart, and check when each is asked for again: a 481 or a NOTIFY ending with deactivated at once (the button keeping its state meanwhile), five quick tries at most and then 30 seconds, a retry-after taken, and an explicit refusal (403, NOTIFY reason invariant) not again until the numbers are set again. And that a registration back after a failure (a PBX restarted) has every subscription asked for anew at once, rather than at its next refresh."""
import random, re, socket, threading, time
from sip_fixture import Phone

DIALOG_INUSE = ('<?xml version="1.0"?><dialog-info xmlns="urn:ietf:params:xml:ns:dialog-info" version="0" state="full">'
                '<dialog id="a1"><state>confirmed</state></dialog></dialog-info>')
SUMMARY = 'Messages-Waiting: no\r\n'
# The numbers watched, each answered by its own rule (see FakePbx.subscribe).
REFRESH_481, ALWAYS_481, FORBIDDEN, INVARIANT, DEACTIVATED, RETRY_AFTER = '201', '202', '203', '204', '205', '206'
# The expiry given to the number whose refresh is refused: libre refreshes at 90% of it.
SHORT_EXPIRES = 8


class FakePbx:
    def __init__(self):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(('127.0.0.1', 0))
        self.sock.settimeout(0.2)
        self.port = self.sock.getsockname()[1]
        self.stop = threading.Event()
        self.lock = threading.Lock()
        # Every SUBSCRIBE that came in: (time, number, Call-ID, a refresh, what it was answered).
        self.subscribes = []
        # Every REGISTER answered: (time, code). What it is answered with, and
        # for how long a registration holds.
        self.registers = []
        self.register_code, self.register_expires = 200, 300
        self.cseq = 0
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def close(self):
        self.stop.set()
        self.thread.join(2)
        self.sock.close()

    def of(self, number):
        with self.lock:
            return [s for s in self.subscribes if s[1] == number]

    def run(self):
        while not self.stop.is_set():
            try:
                data, peer = self.sock.recvfrom(65535)
            except socket.timeout:
                continue
            text = data.decode('utf-8', errors='replace')
            if text.startswith('REGISTER '):
                code, expires = self.register_code, self.register_expires
                if code == 200:
                    self.answer(text, peer, 200, 'OK', tag='reg', extra=self.header(text, 'contact') and
                                f'Contact: {self.header(text, "contact")};expires={expires}\r\nExpires: {expires}\r\n')
                else:
                    self.answer(text, peer, code, 'Server Internal Error', tag='reg')
                with self.lock:
                    self.registers.append((time.monotonic(), code))
            elif text.startswith('SUBSCRIBE '):
                self.subscribe(text, peer)

    @staticmethod
    def header(text, name):
        head = text.split('\r\n\r\n', 1)[0]
        for line in head.split('\r\n')[1:]:
            if ':' in line:
                key, value = line.split(':', 1)
                if key.strip().lower() == name:
                    return value.strip()
        return ''

    def answer(self, text, peer, code, reason, tag, extra=''):
        to = self.header(text, 'to')
        if ';tag=' not in to:
            to += ';tag=' + tag
        reply = (f'SIP/2.0 {code} {reason}\r\nVia: {self.header(text, "via")}\r\nFrom: {self.header(text, "from")}\r\nTo: {to}\r\n'
                 f'Call-ID: {self.header(text, "call-id")}\r\nCSeq: {self.header(text, "cseq")}\r\n{extra}Content-Length: 0\r\n\r\n')
        self.sock.sendto(reply.encode(), peer)
        return to

    def notify(self, text, peer, to, state, body, content_type):
        contact = re.search(r'<([^>]+)>', self.header(text, 'contact')).group(1)
        with self.lock:
            self.cseq += 1
            cseq = self.cseq
        message = (f'NOTIFY {contact} SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{self.port};branch=z9hG4bK{random.getrandbits(48):x}\r\n'
                   f'Max-Forwards: 70\r\nFrom: {to}\r\nTo: {self.header(text, "from")}\r\nCall-ID: {self.header(text, "call-id")}\r\n'
                   f'CSeq: {cseq} NOTIFY\r\nContact: <sip:{self.number(text)}@127.0.0.1:{self.port}>\r\nEvent: {self.header(text, "event")}\r\n'
                   f'Subscription-State: {state}\r\nContent-Type: {content_type}\r\nContent-Length: {len(body.encode())}\r\n\r\n{body}')
        self.sock.sendto(message.encode(), peer)

    def accept(self, text, peer, expires=600):
        to = self.answer(text, peer, 200, 'OK', tag=f'{random.getrandbits(32):x}', extra=f'Expires: {expires}\r\nContact: <sip:{self.number(text)}@127.0.0.1:{self.port}>\r\n')
        if self.header(text, 'event') == 'message-summary':
            self.notify(text, peer, to, f'active;expires={expires}', SUMMARY, 'application/simple-message-summary')
        else:
            self.notify(text, peer, to, f'active;expires={expires}', DIALOG_INUSE, 'application/dialog-info+xml')
        return to

    @staticmethod
    def number(text):
        # Refreshes go to the Contact given, which carries the number too.
        return re.match(r'SUBSCRIBE sip:([^@;>]+)', text).group(1)

    def subscribe(self, text, peer):
        number = self.number(text)
        refresh = ';tag=' in self.header(text, 'to')
        tag = f'{random.getrandbits(32):x}'
        if self.header(text, 'event') == 'message-summary':
            self.accept(text, peer)
            with self.lock:
                self.subscribes.append((time.monotonic(), 'mwi', self.header(text, 'call-id'), refresh, 200))
            return
        if number == REFRESH_481:
            # The PBX's habit: a refresh it has forgotten the subscription of.
            if refresh:
                self.answer(text, peer, 481, 'Call/Transaction Does Not Exist', tag)
                outcome = 481
            else:
                self.accept(text, peer, SHORT_EXPIRES)
                outcome = 200
        elif number == ALWAYS_481:
            self.answer(text, peer, 481, 'Call/Transaction Does Not Exist', tag)
            outcome = 481
        elif number == FORBIDDEN:
            self.answer(text, peer, 403, 'Forbidden', tag)
            outcome = 403
        elif number == RETRY_AFTER:
            self.answer(text, peer, 481, 'Call/Transaction Does Not Exist', tag, extra='Retry-After: 2\r\n')
            outcome = 481
        elif number in (INVARIANT, DEACTIVATED):
            first = not self.of(number)
            to = self.accept(text, peer)
            outcome = 200
            if first:
                # Ended by the server a second later; asked for again it stays.
                reason = 'invariant' if number == INVARIANT else 'deactivated'
                threading.Timer(1.0, self.notify, (text, peer, to, f'terminated;reason={reason}', DIALOG_INUSE,
                                                   'application/dialog-info+xml')).start()
        else:
            self.accept(text, peer)
            outcome = 200
        with self.lock:
            self.subscribes.append((time.monotonic(), number, self.header(text, 'call-id'), refresh, outcome))


def state_of(phone, number):
    return next(p['state'] for p in phone.state()['parking'] if p['number'] == number)


def main():
    pbx = FakePbx()
    phone = None
    try:
        account = dict(server='127.0.0.1', port=pbx.port, extension='1001', auth_user='1001', password='loopback-test-only')
        phone = Phone('subscription-retry', account, extra_config='net_interface 127.0.0.1')
        watched = [REFRESH_481, ALWAYS_481, FORBIDDEN, INVARIANT, DEACTIVATED, RETRY_AFTER]
        start = time.monotonic()
        phone.command('ksip_parking', ','.join(watched))
        # The buttons' states, sampled until the 481 to the refresh has been
        # answered by a new subscription and its NOTIFY.
        seen = {REFRESH_481: [], DEACTIVATED: []}
        deadline = start + SHORT_EXPIRES + 6
        while time.monotonic() < deadline:
            for number in seen:
                seen[number].append(state_of(phone, number))
            time.sleep(0.05)

        subs = pbx.of(REFRESH_481)
        refused = next((i for i, s in enumerate(subs) if s[4] == 481), None)
        assert refused is not None, f'{REFRESH_481}: the refresh never came: {subs}'
        assert refused + 1 < len(subs), f'{REFRESH_481}: not asked for again after the 481: {subs}'
        again = subs[refused + 1]
        assert not again[3] and again[2] != subs[refused][2], f'{REFRESH_481}: the try after the 481 is not a new subscription: {again}'
        gap = again[0] - subs[refused][0]
        assert gap <= 1.5, f'{REFRESH_481}: asked for again {gap:.2f}s after the 481'
        first_inuse = seen[REFRESH_481].index('INUSE')
        assert all(s == 'INUSE' for s in seen[REFRESH_481][first_inuse:]), f'{REFRESH_481}: state not kept: {sorted(set(seen[REFRESH_481]))}'
        print(f'PASS: 更新に 481 が返ると {gap:.2f} 秒で新しい購読を送り、その間も使用中の表示が保たれる')

        subs = pbx.of(DEACTIVATED)
        assert len(subs) >= 2 and subs[1][0] - subs[0][0] <= 2.6, f'{DEACTIVATED}: not asked for again soon after deactivated: {subs}'
        first_inuse = seen[DEACTIVATED].index('INUSE')
        assert all(s == 'INUSE' for s in seen[DEACTIVATED][first_inuse:]), f'{DEACTIVATED}: state not kept: {sorted(set(seen[DEACTIVATED]))}'
        print(f'PASS: NOTIFY が deactivated で終わると {subs[1][0] - subs[0][0] - 1.0:.2f} 秒で購読し直し、表示が保たれる')

        # Five quick tries after the first 481, then quiet until 30 seconds.
        subs = pbx.of(ALWAYS_481)
        assert len(subs) == 6, f'{ALWAYS_481}: {len(subs)} tries in the first {time.monotonic() - start:.0f}s, 6 expected'
        assert subs[-1][0] - subs[0][0] <= 6.5, f'{ALWAYS_481}: the quick tries took {subs[-1][0] - subs[0][0]:.1f}s'
        assert len(pbx.of(RETRY_AFTER)) == 1, f'{RETRY_AFTER}: asked for again before 30s despite the retry-after'
        assert len(pbx.of(FORBIDDEN)) == 1 and len(pbx.of(INVARIANT)) == 1
        print(f'PASS: 481 が続くと、すぐの再試行は 5 回まで（{subs[-1][0] - subs[0][0]:.1f} 秒）で、それ以上は急がない')

        last_quick = subs[-1][0]
        while time.monotonic() < last_quick + 32 and len(pbx.of(ALWAYS_481)) < 7:
            time.sleep(0.2)
        subs = pbx.of(ALWAYS_481)
        assert len(subs) == 7, f'{ALWAYS_481}: no try 30s after the quick ones: {len(subs)}'
        later = subs[6][0] - subs[5][0]
        assert 29 <= later <= 31.5, f'{ALWAYS_481}: the later try came {later:.1f}s after the last quick one'
        time.sleep(1.5)
        assert len(pbx.of(ALWAYS_481)) == 7, f'{ALWAYS_481}: the 30s try set off quick tries again'
        subs = pbx.of(RETRY_AFTER)
        assert len(subs) == 2 and 29 <= subs[1][0] - subs[0][0] <= 31.5, f'{RETRY_AFTER}: {subs}'
        print(f'PASS: その後は {later:.1f} 秒おきに試し続け、その試しが新たな連続の再試行を起こさない。Retry-After 付きの 481 も 30 秒後')

        assert len(pbx.of(FORBIDDEN)) == 1 and state_of(phone, FORBIDDEN) == 'UNKNOWN', f'{FORBIDDEN}: {pbx.of(FORBIDDEN)}'
        assert len(pbx.of(INVARIANT)) == 1 and state_of(phone, INVARIANT) == 'UNKNOWN', f'{INVARIANT}: {pbx.of(INVARIANT)}'
        print('PASS: 403 と、reason=invariant で終わる NOTIFY の後は、購読し直さない')
        phone.command('ksip_parking', ','.join(watched))
        phone.wait(lambda s: len(pbx.of(FORBIDDEN)) == 2 and len(pbx.of(INVARIANT)) == 2, timeout=5)
        print('PASS: 番号を設定し直すと、断られた番号もまた購読する')
    finally:
        if phone:
            phone.close()
        pbx.close()


def recovery():
    """A PBX that refuses one re-registration (as while it restarts) and takes
    the next: the subscriptions are asked for anew when it is back."""
    pbx = FakePbx()
    pbx.register_expires = 30
    phone = None
    try:
        account = dict(server='127.0.0.1', port=pbx.port, extension='1001', auth_user='1001', password='loopback-test-only')
        phone = Phone('subscription-recovery', account, extra_config='net_interface 127.0.0.1\nksip_register_interval 30')
        phone.command('ksip_parking', WATCHED)
        phone.wait(lambda s: state_of(phone, WATCHED) == 'INUSE' and pbx.of('mwi'), timeout=10)
        pbx.register_code = 500
        end = time.monotonic() + 45
        while time.monotonic() < end and not any(code == 500 for _, code in pbx.registers):
            time.sleep(0.2)
        assert any(code == 500 for _, code in pbx.registers), f'no re-registration in 45s: {pbx.registers}'
        pbx.register_code = 200
        refused = max(when for when, code in pbx.registers if code == 500)
        seen = []
        end = time.monotonic() + 90
        while time.monotonic() < end:
            seen.append(state_of(phone, WATCHED))
            back = [when for when, code in pbx.registers if code == 200 and when > refused]
            fresh = [s for s in pbx.of(WATCHED) + pbx.of('mwi') if not s[3] and back and s[0] > back[0]]
            if back and len({s[1] for s in fresh}) == 2:
                break
            time.sleep(0.05)
        assert back, f'the registration did not come back: {pbx.registers}'
        assert len({s[1] for s in fresh}) == 2, f'not asked for anew after the registration came back: {pbx.of(WATCHED)} {pbx.of("mwi")}'
        late = max(s[0] for s in fresh) - back[0]
        assert late <= 1.5, f'asked for anew {late:.2f}s after the registration came back'
        assert all(s == 'INUSE' for s in seen), f'state not kept: {sorted(set(seen))}'
        print(f'PASS: 登録が失敗から戻ると、BLF と留守電の購読を {late:.2f} 秒以内に新しく張り直し、その間も表示が保たれる')
    finally:
        if phone:
            phone.close()
        pbx.close()


# The number watched in the recovery: any the fake PBX simply accepts.
WATCHED = '207'

if __name__ == '__main__':
    main()
    recovery()
