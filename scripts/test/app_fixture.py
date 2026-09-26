"""Set up a disposable UI profile, an auto-answer peer and a caller using the provided accounts."""
import json,pathlib,sys,tempfile,time,winreg
from sip_fixture import PBX,ROOT,Phone,account_with,accounts,ca_certificate,lab,numbers,park_target,park_watch,put_account,talking_source,delete_account
BASE=ROOT/'temp/build/ksip-ui';BASE.mkdir(parents=True,exist_ok=True)
PROFILE='test-ui-ksip'
KEY='Software\\KashiharaCity\\ksip\\Test\\'+PROFILE
TARGET='KSIP/Test/'+PROFILE
if sys.argv[1]=='setup':
    (BASE/'ready').unlink(missing_ok=True)
    configured=accounts()
    # 'setup tls' points the profile at the TLS port and asks for encrypted media,
    # as the extension the PBX requires SRTP on; the plain profile is the first extension.
    secure=len(sys.argv)>2 and sys.argv[2].strip()=='tls'
    where=lab()
    own=account_with('sdes','TLS/SRTPでのアプリのテスト') if secure else configured[0]
    put_account(TARGET,own)
    address=(where['tls_host'],str(where['tls_port'])) if secure else (own['server'],str(own['port']))
    policy=[('server',address[0]),('port',address[1]),('extension',own['extension'])]
    if secure:
        policy+=[('transport','tls'),('media_encryption','sdes'),
                 ('ca_file',str(ca_certificate()))]
    # 'setup buttons' defines custom buttons: a speed dial to the peer, the lab's
    # first park slot, transfers to the playback number, a link, a panel button,
    # do not disturb and voicemail; the numbers come from lab.json.
    if len(sys.argv)>2 and sys.argv[2].strip()=='buttons':
        n=numbers();slot=n['park_slots'][0]
        policy+=[('button_1_title','Peer'),('button_1_kind','dial'),('button_1_number',configured[1]['extension']),
                 # The pickup target is named as well, so that the field is exercised; it is the slot itself here.
                 ('button_2_title','Park'),('button_2_kind','park'),('button_2_number',park_watch(slot)),('button_2_transfer',park_target(slot)),('button_2_pickup',slot),
                 ('button_3_title','Playback'),('button_3_kind','transfer'),('button_3_number',n['playback']),
                 # The same player named as a full URI in angle brackets, as some PBXs want it.
                 ('button_4_title','Player URI'),('button_4_kind','transfer'),('button_4_number','<sip:'+n['playback']+'@'+where['server']+'>'),
                 ('button_5_title','Directory'),('button_5_kind','open'),('button_5_number','https://example.invalid/extensions'),
                 # A button in the panel beside the phone, which makes the window twice as wide.
                 ('button_7_title','Panel player'),('button_7_kind','dial'),('button_7_number',n['playback']),
                 ('button_8_kind','dnd'),
                 ('button_9_kind','mwi'),('button_9_number',n['voicemail']),
                 # A dial without BLF: the peer's extension written with a separator, as an outside line would be.
                 ('button_10_title','Speed'),('button_10_kind','speed'),('button_10_number',configured[1]['extension'][:2]+'-'+configured[1]['extension'][2:])]
    with winreg.CreateKey(winreg.HKEY_CURRENT_USER,KEY) as key:
        general=dict(sip_port=17560,rtp_port=17700,microphone='default',speaker='default',aec=True)
        # The buttons profile also asks for the window to go to the tray ten seconds after a call.
        if len(sys.argv)>2 and sys.argv[2].strip()=='buttons':general['tray_after_call']=10
        # 'setup missing-device' saves a microphone that no machine has.
        if len(sys.argv)>2 and sys.argv[2].strip()=='missing-device':general['microphone']='{0.0.1.00000000}.{00000000-0000-0000-0000-000000000000}'
        winreg.SetValueEx(key,'Settings',0,winreg.REG_SZ,json.dumps(general))
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
elif sys.argv[1]=='voicemail':
    # Leaves a message in the phone's own box: the lab's direct prefix + 100X
    # records for 100X, and the box's message-summary then reports one more.
    # The talker keeps talking and stays long enough for a PBX that plays a
    # greeting first and drops a message with nothing said in it (3CX).
    configured=accounts()
    talker=Phone('ui-talker',configured[1],18566,19000,audio_source=talking_source())
    try:
        call=talker.action('dial',value=numbers()['voicemail_direct_prefix']+configured[0]['extension'])
        talker.wait(lambda s:any(c['id']==call and c['state']=='ESTABLISHED' for c in s['calls']),20)
        time.sleep(15)
        talker.action('hangup',call)
        time.sleep(1)
    finally:talker.close()
elif sys.argv[1]=='quick':
    # Calls this phone from the second account and hangs up after the given
    # seconds, the given number of times: 'quick 0.06 3' cancels three calls
    # inside one poll interval; under do not disturb the phone refuses them.
    hold=float(sys.argv[2]);times=int(sys.argv[3])
    configured=accounts()
    quick=Phone('ui-quick',configured[1],18574,19100)
    try:
        for _ in range(times):
            call=quick.action('dial',value=configured[0]['extension'])
            time.sleep(hold)
            try:quick.action('hangup',call)
            except Exception:pass
            quick.wait(lambda s:not s['calls'],timeout=10)
            time.sleep(1.2)
    finally:quick.close()
elif sys.argv[1]=='group':
    # Rings the lab's ring group, which is the phone and the third account
    # together, from the peer's account. The third account takes the call
    # after the phone has rung a moment, so the PBX cancels the phone's ring
    # with a Reason header saying the call was completed elsewhere.
    # The result goes into a file, as the caller's does: the exit code of a
    # process Windows PowerShell started is often not available to it.
    result=BASE/'group-result.txt';result.unlink(missing_ok=True)
    configured=accounts()
    taker=Phone('ui-taker',configured[2],18570,19200)
    caller=Phone('ui-group-caller',configured[1],18572,19300)
    try:
        call=caller.action('dial',value=numbers()['group'])
        ringing=taker.wait(lambda s:any(c['state']=='INCOMING' for c in s['calls']),20)
        time.sleep(3)
        for c in ringing['calls']:
            if c['state']=='INCOMING':taker.action('answer',c['id'])
        caller.wait(lambda s:any(c['id']==call and c['state']=='ESTABLISHED' for c in s['calls']),20)
        time.sleep(2)
        caller.action('hangup',call)
        time.sleep(1)
        result.write_text('TAKEN_ELSEWHERE')
    finally:
        caller.close();taker.close()
elif sys.argv[1]=='lab':
    # The lab's conventions for the PowerShell tests: which PBX, its numbers and
    # the extensions in use (never the passwords).
    print(json.dumps(dict(pbx=PBX,numbers=numbers(),extensions=[a['extension'] for a in accounts()],server=lab()['server'],features=lab().get('features',{})),ensure_ascii=False))
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
