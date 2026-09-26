"""OSRTP (RFC 8643) against a PBX extension that offers keys on RTP/AVP: encrypted both ways, with KSIP set to osrtp."""
import re,time
from sip_fixture import Phone,accounts,account_with,ca_certificate,connect,has_account,lab,numbers

def main():
    if not has_account('osrtp'):
        print('SKIP: この PBX には OSRTP の内線（encryption: osrtp）が無い',flush=True)
        return
    where=lab()
    ca=str(ca_certificate())
    own=dict(account_with('osrtp','OSRTPのテスト'),server=where['tls_host'],port=where['tls_port'])
    configured=accounts()
    a=caller=None
    try:
        # KSIP's osrtp is baresip's srtp: keys on RTP/AVP, over TLS as RFC 8643 asks.
        a=Phone('osrtp-a',own,18578,19020,transport='TLS',mediaenc='srtp',ca_file=ca)
        call=a.action('dial',value=numbers()['playback'])
        a.wait(lambda s:any(c['id']==call and c['state']=='ESTABLISHED' for c in s['calls']),timeout=20)
        time.sleep(1)
        match=re.search(r'srtp: audio: SRTP is Enabled \(cryptosuite=(\S+)\)',a.log_text())
        assert match, 'OSRTPで提示した発信が暗号化されていません（PBXが鍵を返していない）'
        print(f'PASS: OSRTPの提示に PBX が鍵を返し、発信が暗号化された (cryptosuite={match[1]})',flush=True)
        a.action('hangup',call);a.wait(lambda s:not s['calls'])
        # The PBX's own OSRTP offer to this extension is answered with keys as well.
        peer=configured[1] if configured[1]['extension']!=own['extension'] else configured[0]
        caller=Phone('osrtp-caller',peer,18580,19030)
        _,remote=connect(caller,a,own['extension'])
        time.sleep(1)
        assert len(re.findall(r'SRTP is Enabled',a.log_text()))>=2, 'PBX からの OSRTP の提示に鍵を返した着信が暗号化されていません'
        print('PASS: PBX からの OSRTP の提示にも鍵を返し、着信が暗号化された',flush=True)
        a.action('hangup',remote);caller.wait(lambda s:not s['calls']);a.wait(lambda s:not s['calls'])
    finally:
        if a:a.close()
        if caller:caller.close()
    print('PASS: OSRTP both ways against an extension that offers keys on RTP/AVP',flush=True)

if __name__=='__main__':main()
