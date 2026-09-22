"""Set up a disposable UI profile, an auto-answer peer and a caller using the provided accounts."""
import json,pathlib,sys,tempfile,time,winreg
from sip_fixture import ROOT,Phone,accounts,lab,put_account,delete_account
BASE=ROOT/'temp/build/ksip-ui';BASE.mkdir(parents=True,exist_ok=True)
PROFILE='test-ui-ksip'
KEY='Software\\KashiharaCity\\ksip\\Test\\'+PROFILE
TARGET='KSIP/Test/'+PROFILE
if sys.argv[1]=='setup':
    (BASE/'ready').unlink(missing_ok=True)
    configured=accounts();put_account(TARGET,configured[0])
    # 'setup tls' points the profile at the TLS port and asks for encrypted media.
    secure=len(sys.argv)>2 and sys.argv[2].strip()=='tls'
    where=lab()
    address=(where['tls_host'],str(where['tls_port'])) if secure else (configured[0]['server'],str(configured[0]['port']))
    policy=[('server',address[0]),('port',address[1]),('extension',configured[0]['extension'])]
    if secure:
        policy+=[('transport','tls'),('media_encryption','sdes'),
                 ('ca_file',str(ROOT/'local-asterisk/LocalCA.crt'))]
    # 'setup buttons' defines three custom buttons: a speed dial to the peer, a
    # park slot the lab parks on *701 and picks up on 701, and a transfer to 9001.
    if len(sys.argv)>2 and sys.argv[2].strip()=='buttons':
        policy+=[('button_1_title','Peer'),('button_1_kind','dial'),('button_1_number',configured[1]['extension']),
                 ('button_2_title','Park'),('button_2_kind','park'),('button_2_number','701'),('button_2_transfer','*701'),
                 ('button_3_title','Playback'),('button_3_kind','transfer'),('button_3_number','9001')]
    with winreg.CreateKey(winreg.HKEY_CURRENT_USER,KEY) as key:
        winreg.SetValueEx(key,'Settings',0,winreg.REG_SZ,json.dumps(dict(sip_port=17560,rtp_port=17700,microphone='default',speaker='default',aec=True)))
        for name,value in policy:
            winreg.SetValueEx(key,name,0,winreg.REG_SZ,value)
    (BASE/'peer-extension.txt').write_text(configured[1]['extension'])
elif sys.argv[1]=='peer':
    done=BASE/'done';done.unlink(missing_ok=True);(BASE/'ready').unlink(missing_ok=True)
    peer=Phone('ui-peer',accounts()[1],18562,18800)
    try:
        (BASE/'ready').write_text('ready')
        deadline=time.monotonic()+180
        while time.monotonic()<deadline and not done.exists():
            state=peer.state()
            for c in state['calls']:
                if c['state']=='INCOMING':peer.action('answer',c['id'])
            time.sleep(.15)
    finally:peer.close()
elif sys.argv[1]=='caller':
    done=BASE/'caller-done';done.unlink(missing_ok=True)
    (BASE/'caller-ready').unlink(missing_ok=True);(BASE/'caller-result.txt').unlink(missing_ok=True)
    configured=accounts()
    caller=Phone('ui-caller',configured[1],18564,18900)
    try:
        call=caller.action('dial',value=configured[0]['extension'])
        (BASE/'caller-ready').write_text('dialing')
        established=False;deadline=time.monotonic()+40
        while time.monotonic()<deadline and not done.exists():
            if any(c['id']==call and c['state']=='ESTABLISHED' for c in caller.state()['calls']):established=True
            time.sleep(.15)
        (BASE/'caller-result.txt').write_text('ESTABLISHED' if established else 'NOT_ESTABLISHED')
    finally:caller.close()
elif sys.argv[1]=='cleanup':
    delete_account(TARGET)
    try:winreg.DeleteKey(winreg.HKEY_CURRENT_USER,KEY)
    except FileNotFoundError:pass
elif sys.argv[1]=='stop-engine':
    import socket,re
    path=pathlib.Path(tempfile.gettempdir())/'ksip-profile'/PROFILE/'config'
    if path.exists():
        match=re.search(r'ctrl_tcp_listen 127.0.0.1:(\d+)',path.read_text())
        if match:
            try:
                with socket.create_connection(('127.0.0.1',int(match[1])),timeout=2) as sock:
                    data=b'{"command":"quit","token":"cleanup"}'
                    sock.sendall(str(len(data)).encode()+b':'+data+b',')
                time.sleep(1)
            except OSError:pass
