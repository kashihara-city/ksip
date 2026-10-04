"""With "rtp_ports 0" and no pair of ports to be had from the system (every odd UDP port of the dynamic range taken here, so that no RTCP socket lands next to an RTP one), the call's SDP says where RTCP is (a=rtcp, RFC 3605: the stream.c change in scripts/build/patch-baresip.py) and the engine's RTCP comes from that port. A fake PBX on 127.0.0.1 answers the INVITE and keeps the offer."""
from pathlib import Path
import importlib.util, json, re, socket, subprocess, sys
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/test'))
from sip_fixture import Phone
spec = importlib.util.spec_from_file_location('loopback_sendonly', ROOT / 'scripts/test/loopback-sendonly.py')
sendonly = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sendonly)


def dynamic_range():
    """The UDP ports Windows hands out for port 0: start and end (exclusive)."""
    out = subprocess.run(['netsh', 'int', 'ipv4', 'show', 'dynamicport', 'udp'], capture_output=True, text=True, encoding='utf-8', errors='replace').stdout
    numbers = [int(x) for x in re.findall(r':\s*(\d+)', out)]
    return (numbers[0], numbers[0] + numbers[1]) if len(numbers) >= 2 else (49152, 65536)


def main():
    (ROOT / 'temp/reports').mkdir(parents=True, exist_ok=True)
    start, end = dynamic_range()
    # Every odd port of the range, held for the test: a second socket the
    # system hands out can then never be the port next to an even first one.
    taken = []
    for port in range(start | 1, end, 2):
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            s.bind(('0.0.0.0', port))
            taken.append(s)
        except OSError:
            s.close()
    pbx = sendonly.FakePbx('sendrecv')
    account = dict(server='127.0.0.1', port=pbx.port, extension='1001', auth_user='1001', password='fake-only')
    phone = None
    result = {'range': [start, end], 'odd_ports_taken': len(taken)}
    try:
        phone = Phone('rtcp-port', account, sip_port=0, rtp_port=0, codecs=('g711',),
                      extra_config='net_interface 127.0.0.1\nfilter_registrar UDP,TCP,TLS')
        call = phone.action('dial', value='1002')
        phone.wait(lambda s: any(c['id'] == call and c['state'] == 'ESTABLISHED' for c in s['calls']))
        # The engine's RTCP reports come every few seconds.
        result['counts'] = pbx.count(12)
        offer = pbx.invites[-1]
        result['offer_media'] = [l for l in offer.split('\r\n') if l.startswith(('m=', 'a=rtcp'))]
        result['log'] = [l.strip()[:200] for l in phone.log_text().splitlines() if 'RTCP is on port' in l]
        phone.action('hangup', call)
        phone.wait(lambda s: not s['calls'], timeout=10)
    finally:
        if phone:
            phone.close()
        pbx.close()
        for s in taken:
            s.close()
    (ROOT / 'temp/reports/loopback-rtcp-port.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    rtp = int(re.search(r'm=audio (\d+) ', offer).group(1))
    said = re.search(r'a=rtcp:(\d+)', offer)
    counts = result['counts']
    assert counts['rtcp'] >= 1, f'no RTCP from the engine in twelve seconds: {counts}'
    if said is None and counts['rtcp_from'] == [rtp + 1]:
        print(f'SKIP: the system found a pair of ports after all (RTP {rtp}, RTCP {rtp + 1}) with {len(taken)} odd ports of {start}-{end} taken; nothing to say in the SDP')
        return
    assert said, f'RTCP cannot be next to RTP {rtp}, and the offer does not say where it is: {result["offer_media"]}'
    rtcp = int(said.group(1))
    assert rtcp != rtp + 1, f'a=rtcp:{rtcp} says the usual pair: not what the test set up'
    assert counts['rtcp_from'] == [rtcp], f'the RTCP came from {counts["rtcp_from"]}, the SDP says {rtcp}'
    assert result['log'], 'the engine did not warn of RTCP not next to RTP'
    print(f'PASS: with no pair of ports to be had, the offer says a=rtcp:{rtcp} beside RTP {rtp}, and {counts["rtcp"]} RTCP packet(s) came from that port')


if __name__ == '__main__':
    main()
