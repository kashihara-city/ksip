"""The other party puts the call on hold: what the PBX sends meanwhile (its hold music) reaches this side, whose RTP ports the system picked as the app's are and which has no firewall rule, whichever way the PBX does it: playing the music into the stream as it is (Asterisk, FreeSWITCH) or offering a=sendonly first (3CX), after which this side sends no RTP and has to open the way in itself."""
import re, time
from sip_fixture import ROOT, Phone, accounts, PBX


def receive_rates(phone):
    """The receive bit rates the engine printed, in order."""
    return [int(rx) for _, rx in re.findall(r'\[\d:\d\d:\d\d\] audio=(\d+)/(\d+) \(bit/s\)', phone.log_text())]


def main():
    acc = accounts()
    a = Phone('hold-media-a', acc[0], rtp_port=0, extra_config='ksip_detail_log yes')
    b = Phone('hold-media-b', acc[1])
    try:
        call_a = a.action('dial', value=acc[1]['extension'])
        b.wait(lambda s: any(c['state'] == 'INCOMING' for c in s['calls']), timeout=20)
        call_b = next(c['id'] for c in b.state()['calls'] if c['state'] == 'INCOMING')
        b.action('answer', call_b)
        a.wait(lambda s: any(c['id'] == call_a and c['state'] == 'ESTABLISHED' for c in s['calls']), timeout=20)
        time.sleep(3)
        before = len(a.log_text())
        b.action('hold', call_b)
        # The hold music has to be heard within ten seconds of the hold.
        heard, end = 0, time.monotonic() + 10
        while time.monotonic() < end and not heard:
            time.sleep(1)
            rates = [int(rx) for _, rx in re.findall(r'\[\d:\d\d:\d\d\] audio=(\d+)/(\d+) \(bit/s\)', a.log_text()[before:])]
            heard = max(rates[-3:], default=0)
        during = a.log_text()[before:]
        offered = 'sendonly' if 'a=sendonly' in during else 'sendrecv'
        opened = sum(1 for l in during.splitlines() if 'keep the way in open' in l)
        assert heard >= 4000, f'little or nothing received from {PBX} while the other party held the call (it offered {offered}): {heard} bit/s'
        # A PBX that had this side stop sending must have had the way in opened from here.
        assert offered != 'sendonly' or opened >= 1, 'the PBX offered sendonly and this side did not open the way in'
        b.action('resume', call_b)
        time.sleep(4)
        after = receive_rates(a)[-3:]
        assert max(after, default=0) > 0, f'nothing received after the resume: {after}'
        assert any(c['id'] == call_a and c['state'] == 'ESTABLISHED' for c in a.state()['calls'])
        a.action('hangup', call_a)
        a.wait(lambda s: not s['calls'], timeout=10)
    finally:
        a.close()
        b.close()
    print(f"PASS: {PBX} の相手の保留中に保留音が届く（PBX は {offered}、受信 {heard} bit/s、こちらから戻り道を開いた回数 {opened}）")


if __name__ == '__main__':
    main()
