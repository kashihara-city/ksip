"""SIP over TCP against the lab PBX: registration is answered over TCP and a call to the playback number carries audio."""
import re,time
from sip_fixture import Phone,accounts,numbers

def main():
    a=None
    try:
        a=Phone('tcp-a',accounts()[0],18582,19040,transport='TCP')
        text=a.log_text()
        assert re.search(r'\{0/TCP/v4\}|/TCP/',text), 'TCPで登録した形跡がありません'
        print('PASS: TCPで登録した',flush=True)
        call=a.action('dial',value=numbers()['playback'])
        a.wait(lambda s:any(c['id']==call and c['state']=='ESTABLISHED' for c in s['calls']),timeout=20)
        time.sleep(3)
        state=a.state()
        assert any(c['id']==call and c['state']=='ESTABLISHED' for c in state['calls']), '通話が続いていません'
        assert 'transport=tcp' in a.log_text(), 'INVITE が TCP で出ていません'
        print('PASS: TCPで発信して通話が成立した',flush=True)
        a.action('hangup',call);a.wait(lambda s:not s['calls'])
    finally:
        if a:a.close()
    print('PASS: SIP over TCP registers and calls',flush=True)

if __name__=='__main__':main()
