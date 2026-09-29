"""Keepalives against the lab PBX: over UDP, TCP and TLS a blank line goes to the registrar at the chosen interval while the registration holds, and none goes when they are off."""
import re,time
from sip_fixture import Phone,accounts,ca_certificate,lab

# Short, so that the test sees more than one go by; the app's own range starts at 10.
INTERVAL=10

def config(mode,interval=INTERVAL):
    return f'ksip_keepalive {mode}\nksip_keepalive_interval {interval}'

def sent(name,account,transport,**extra):
    phone=None
    try:
        phone=Phone(name,account,transport=transport,extra_config=config('crlf'),**extra)
        # The first goes one interval after the registration, the next ones after it.
        time.sleep(INTERVAL*2.5)
        text=phone.log_text()
        told=re.search(r'ksip: keepalive \(CRLF\) every (\d+) s to (\S+) over (\S+)',text)
        assert told,f'{transport}: キープアライブを送った記録がエンジンのログにありません'
        assert told.group(1)==str(INTERVAL),f'{transport}: 間隔が {told.group(1)} 秒になっています'
        assert told.group(3).upper()==transport,f'{transport}: {told.group(3)} で送っています'
        assert 'could not be sent' not in text,f'{transport}: 送れなかった記録があります'
        assert phone.state()['registration']=='REGISTER_OK',f'{transport}: 登録が外れました'
        print(f'PASS: {transport}: {INTERVAL}秒ごとに登録先 {told.group(2)} へ空行を送り、登録は保たれた',flush=True)
    finally:
        if phone:phone.close()

def main():
    plain=accounts()[0]
    sent('keepalive-udp',plain,'UDP')
    sent('keepalive-tcp',plain,'TCP')
    # Over TLS the registrar is named as its certificate is, on its TLS port.
    sent('keepalive-tls',dict(plain,server=lab()['tls_host'],port=lab()['tls_port']),'TLS',ca_file=str(ca_certificate()))
    phone=None
    try:
        phone=Phone('keepalive-off',plain,extra_config=config('off'))
        time.sleep(INTERVAL*1.5)
        assert 'ksip: keepalive' not in phone.log_text(),'送らない設定なのにキープアライブを送っています'
        assert phone.state()['registration']=='REGISTER_OK'
        print('PASS: 送らない設定では送らない',flush=True)
    finally:
        if phone:phone.close()
    print('PASS: keepalives go at the chosen interval over UDP, TCP and TLS, and not when off',flush=True)

if __name__=='__main__':main()
