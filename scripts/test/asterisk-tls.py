"""SIP over TLS with a verified certificate and SDES-SRTP media, against the user-provided Asterisk."""
import json,re,time
from sip_fixture import ROOT,Phone,accounts,connect,dtls_account,lab,version

CA=ROOT/'local-asterisk/LocalCA.crt'
SERVER=lab()['tls_host']

def engine_log(phone):
    phone.log.flush()
    return (phone.dir/'engine.log').read_bytes().decode('utf-8','replace').replace('\r','\n')

def main():
    (ROOT/'temp/reports').mkdir(parents=True,exist_ok=True)
    assert CA.is_file(), 'local-asterisk/LocalCA.crt が必要です'
    configured=accounts()
    # The certificate carries the name, so the account must use it too.
    secure=[dict(item,server=SERVER,port=5061) for item in configured]
    report={}
    a=b=None
    try:
        a=Phone('tls-a',secure[0],18570,18980,transport='TLS',mediaenc='srtp',ca_file=str(CA))
        text=engine_log(a)
        assert 'REGISTER_FAIL' not in text, 'TLSで登録できませんでした'
        assert re.search(r'\{0/TLS/v4\}|/TLS/',text), f'TLSで登録した形跡がありません'
        print('PASS: 証明書を検証してTLSで登録した',flush=True)
        report['register']='tls'

        b=Phone('tls-b',secure[1],18572,18990,transport='TLS',mediaenc='srtp',ca_file=str(CA))
        first,remote=connect(a,b,secure[1]['extension'])
        time.sleep(1)
        for phone,name in ((a,'発信側'),(b,'着信側')):
            text=engine_log(phone)
            match=re.search(r'srtp: audio: SRTP is Enabled \(cryptosuite=(\S+)\)',text)
            assert match, f'{name}のメディアが暗号化されていません'
            report.setdefault('cryptosuite',match[1])
        print(f"PASS: 両端でSRTPが有効 (cryptosuite={report['cryptosuite']})",flush=True)
        a.action('hangup',first)
        b.wait(lambda s:not s['calls'])
        a.wait(lambda s:not s['calls'])
    finally:
        if a:a.close()
        if b:b.close()
    # The account marked dtls in lab.json is the DTLS endpoint on the lab
    # server (use_avpf), where the keys are exchanged on the media path
    # instead of in the signalling.
    dtls=None
    try:
        dtls=Phone('tls-dtls',dict(dtls_account(),server=SERVER,port=5061),18576,19010,transport='TLS',mediaenc='dtls_srtp',ca_file=str(CA))
        call=dtls.action('dial',value='9001')
        dtls.wait(lambda s:any(c['id']==call and c['state']=='ESTABLISHED' for c in s['calls']),timeout=25)
        time.sleep(3)
        text=engine_log(dtls)
        match=re.search(r'DTLS-SRTP complete \(audio/RTP\) Profile=(\S+)',text)
        assert match, 'DTLS-SRTPが完了していません'
        assert 'verified SHA-256 fingerprint OK' in text, '指紋が検証されていません'
        print(f'PASS: DTLS-SRTPで鍵交換して通話した (Profile={match[1]})',flush=True)
        report['dtls']=match[1]
        dtls.action('hangup',call)
        dtls.wait(lambda s:not s['calls'])
    finally:
        if dtls:dtls.close()

    # A wrong trust anchor must be refused, or verification would be theatre.
    bad=ROOT/'temp/build/wrong-ca.pem'
    bad.parent.mkdir(parents=True,exist_ok=True)
    bad.write_bytes((ROOT/lab()['server_certificate']).read_bytes())
    rejected=None
    try:
        rejected=Phone('tls-bad',secure[0],18574,19000,transport='TLS',mediaenc='srtp',ca_file=str(bad))
        text=engine_log(rejected)
        assert 'REGISTER_OK' not in text, '誤ったCAでも登録できてしまいました'
    except RuntimeError:
        pass
    finally:
        if rejected:rejected.close()
    print('PASS: 認証局が違う証明書は拒否される',flush=True)
    report['rejects_wrong_ca']=True
    (ROOT/f'temp/reports/asterisk-tls-v{version()}.json').write_text(json.dumps(report,indent=2)+'\n')
    print('PASS: SIP over TLS with a verified certificate, SDES and DTLS media',flush=True)

if __name__=='__main__':main()
