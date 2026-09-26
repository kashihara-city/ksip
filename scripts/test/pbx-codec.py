"""Codec negotiation against the lab PBX: Opus, G.722 and the fallback."""
import json,re,time
from sip_fixture import PBX,ROOT,Phone,accounts,connect,skip_unless,version

def negotiated(phone):
    """The encoder and decoder baresip settled on, read from its own log."""
    text=phone.log_text()
    encode=re.findall(r'Set audio encoder: (\S+) (\d+)Hz',text)
    decode=re.findall(r'Set audio decoder: (\S+) (\d+)Hz',text)
    assert encode and decode,'no codec was set'
    return encode[-1],decode[-1]

def main():
    (ROOT/'temp/reports').mkdir(parents=True,exist_ok=True)
    configured=accounts();report={}
    a=b=narrow=None
    try:
        # Both sides offer the wideband codecs, so the PBX should bridge Opus.
        a=Phone('codec-a',configured[0],18566,18920)
        b=Phone('codec-b',configured[1],18568,18960)
        first,remote=connect(a,b,configured[1]['extension'])
        time.sleep(1)
        encode,decode=negotiated(a)
        assert encode[0]=='opus' and decode[0]=='opus',f'expected opus, got {encode} {decode}'
        assert encode[1]=='48000',f'expected 48 kHz, got {encode[1]}'
        peer_encode,_=negotiated(b)
        assert peer_encode[0]=='opus',f'peer did not use opus: {peer_encode}'
        print(f'PASS: 両端がOpusで通話 ({encode[0]} {encode[1]}Hz)',flush=True)
        report['opus']=f'{encode[0]}/{encode[1]}'
        a.action('hangup',first)
        b.wait(lambda s:not s['calls'])
        a.wait(lambda s:not s['calls'])
    finally:
        if a:a.close()
        if b:b.close()
    try:
        # A PBX that negotiates each leg on its own transcodes between them, so
        # a narrowband peer does not pull this side down.
        skip_unless('transcoding','相手がG.711でもこちらがOpusのままになるのはPBXが変換するときだけ')
        a=Phone('codec-a',configured[0],18566,18920)
        narrow=Phone('codec-narrow',configured[1],18568,18960,codecs=('g711',))
        first,remote=connect(a,narrow,configured[1]['extension'])
        time.sleep(1)
        encode,_=negotiated(a)
        peer_encode,_=negotiated(narrow)
        assert encode[0]=='opus',f'this side left opus: {encode}'
        assert peer_encode[0] in ('PCMU','PCMA'),f'expected G.711 on the peer, got {peer_encode}'
        print(f'PASS: 相手がG.711でもこちらはOpusのまま（{encode[0]} / {peer_encode[0]}）',flush=True)
        report['mixed']=f'{encode[0]}+{peer_encode[0]}'
        a.action('hangup',first)
        narrow.wait(lambda s:not s['calls'])
    finally:
        if a:a.close()
        if narrow:narrow.close()
    try:
        # Without Opus the next one in the offer is G.722, still wideband.
        a=Phone('codec-a',configured[0],18566,18920,codecs=('g711','libg722'))
        b=Phone('codec-b',configured[1],18568,18960,codecs=('g711','libg722'))
        first,remote=connect(a,b,configured[1]['extension'])
        time.sleep(1)
        encode,decode=negotiated(a)
        assert encode[0]=='G722',f'expected G722, got {encode}'
        print(f'PASS: Opusが無ければG.722を使う ({encode[0]} {encode[1]}Hz)',flush=True)
        report['g722']=f'{encode[0]}/{encode[1]}'
        a.action('hangup',first)
        b.wait(lambda s:not s['calls'])
    finally:
        if a:a.close()
        if b:b.close()
    (ROOT/f'temp/reports/pbx-codec-{PBX}-v{version()}.json').write_text(json.dumps(report,indent=2)+'\n')
    print('PASS: PBX codec negotiation, opus first and falling back in order',flush=True)

if __name__=='__main__':main()
