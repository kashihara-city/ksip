"""SIP over TLS with a verified certificate, then SRTP required (SDES) and DTLS-SRTP, against the lab PBX."""
import json,re,time
from sip_fixture import PBX,ROOT,Phone,accounts,account_with,ca_certificate,connect,has_account,lab,numbers,version

CA=ca_certificate()
SERVER=lab()['tls_host']
TLS_PORT=lab()['tls_port']

def over_tls(account):
    """The account addressed by the name on the certificate, on the TLS port."""
    return dict(account,server=SERVER,port=TLS_PORT)

def main():
    (ROOT/'temp/reports').mkdir(parents=True,exist_ok=True)
    configured=accounts()
    strict=over_tls(account_with('sdes','SRTP必須の内線'))
    report={}
    # KSIP's sdes is baresip's srtp-mand: keys on RTP/SAVP, and no call without them.
    # The extension the PBX requires SRTP on is called from and called into.
    a=caller=None
    try:
        a=Phone('tls-a',strict,18570,18980,transport='TLS',mediaenc='srtp-mand',ca_file=str(CA))
        text=a.log_text()
        assert 'REGISTER_FAIL' not in text, 'TLSで登録できませんでした'
        assert re.search(r'\{0/TLS/v4\}|/TLS/',text), 'TLSで登録した形跡がありません'
        print('PASS: 証明書を検証してTLSで登録した',flush=True)
        report['register']='tls'
        call=a.action('dial',value=numbers()['playback'])
        a.wait(lambda s:any(c['id']==call and c['state']=='ESTABLISHED' for c in s['calls']),timeout=20)
        time.sleep(1)
        match=re.search(r'srtp: audio: SRTP is Enabled \(cryptosuite=(\S+)\)',a.log_text())
        assert match, '発信した通話のメディアが暗号化されていません'
        report['cryptosuite']=match[1]
        a.action('hangup',call);a.wait(lambda s:not s['calls'])
        print(f"PASS: SRTP必須の内線から発信して暗号化された (cryptosuite={match[1]})",flush=True)
        # The PBX offers RTP/SAVP with keys to that extension, whoever calls it.
        caller=Phone('tls-caller',configured[1],18572,18990)
        _,remote=connect(caller,a,strict['extension'])
        time.sleep(1)
        assert len(re.findall(r'SRTP is Enabled',a.log_text()))>=2, '着信した通話のメディアが暗号化されていません'
        print('PASS: SRTP必須の内線への着信も暗号化された',flush=True)
        a.action('hangup',remote);caller.wait(lambda s:not s['calls']);a.wait(lambda s:not s['calls'])
    finally:
        if a:a.close()
        if caller:caller.close()
    # The DTLS extension: the keys are exchanged on the media path, not in the signalling.
    # A PBX without DTLS-SRTP (3CX) has no such extension, and this step alone is skipped.
    if not has_account('dtls'):
        print(f'NOTE: DTLS-SRTPの内線が無いので鍵交換の確認は飛ばす（{PBX} の lab.json の accounts に "encryption": "dtls" が無い）',flush=True)
        report['dtls']='skipped'
    else:
        dtls=None
        try:
            dtls=Phone('tls-dtls',over_tls(account_with('dtls','DTLS-SRTPの内線')),18576,19010,transport='TLS',mediaenc='dtls_srtp',ca_file=str(CA))
            call=dtls.action('dial',value=numbers()['playback'])
            dtls.wait(lambda s:any(c['id']==call and c['state']=='ESTABLISHED' for c in s['calls']),timeout=25)
            time.sleep(3)
            text=dtls.log_text()
            match=re.search(r'DTLS-SRTP complete \(audio/RTP\) Profile=(\S+)',text)
            assert match, 'DTLS-SRTPが完了していません'
            assert 'verified SHA-256 fingerprint OK' in text, '指紋が検証されていません'
            print(f'PASS: DTLS-SRTPで鍵交換して通話した (Profile={match[1]})',flush=True)
            report['dtls']=match[1]
            dtls.action('hangup',call);dtls.wait(lambda s:not s['calls'])
        finally:
            if dtls:dtls.close()
    # A wrong trust anchor must be refused, or verification would be theatre.
    bad=ROOT/'temp/build/wrong-ca.pem'
    bad.parent.mkdir(parents=True,exist_ok=True)
    bad.write_bytes((ROOT/lab()['server_certificate']).read_bytes())
    rejected=None
    try:
        rejected=Phone('tls-bad',strict,18574,19000,transport='TLS',mediaenc='srtp-mand',ca_file=str(bad))
        assert 'REGISTER_OK' not in rejected.log_text(), '誤ったCAでも登録できてしまいました'
    except RuntimeError:
        pass
    finally:
        if rejected:rejected.close()
    print('PASS: 認証局が違う証明書は拒否される',flush=True)
    report['rejects_wrong_ca']=True
    (ROOT/f'temp/reports/pbx-tls-{PBX}-v{version()}.json').write_text(json.dumps(report,indent=2)+'\n')
    print('PASS: SIP over TLS with a verified certificate, SRTP required, and DTLS media',flush=True)

if __name__=='__main__':main()
