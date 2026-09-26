"""Isolated KSIP test helpers. Never print credentials or write passwords to disk."""
from pathlib import Path
import ctypes as C
from ctypes import wintypes as W
import json, os, re, socket, subprocess, threading, time

ROOT = Path(__file__).resolve().parents[2]
class Credential(C.Structure):
    _fields_ = [('Flags',W.DWORD),('Type',W.DWORD),('TargetName',W.LPWSTR),('Comment',W.LPWSTR),
                ('LastWritten',W.FILETIME),('CredentialBlobSize',W.DWORD),('CredentialBlob',C.POINTER(C.c_ubyte)),
                ('Persist',W.DWORD),('AttributeCount',W.DWORD),('Attributes',C.c_void_p),('TargetAlias',W.LPWSTR),('UserName',W.LPWSTR)]
advapi = C.WinDLL('advapi32',use_last_error=True)
advapi.CredWriteW.argtypes=[C.POINTER(Credential),W.DWORD]
advapi.CredDeleteW.argtypes=[W.LPCWSTR,W.DWORD,W.DWORD]
def version():
    text=(ROOT/'src-tauri/Cargo.toml').read_text(encoding='utf-8')
    return re.search(r'(?m)^version\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"',text)[1]
PBX = os.environ.get('KSIP_TEST_PBX', 'local-asterisk')
LAB = ROOT / 'test-pbx' / PBX / 'lab.json'
# What the lab's dialplan provides, by the numbers the development Asterisk
# uses; a lab.json "numbers" object overrides any of them.
DEFAULT_NUMBERS = dict(playback='9001', park_slots=['701', '702', '703'], park_prefix='*',
                       group='7000', voicemail='*97', voicemail_direct_prefix='8', unassigned='1999')
def lab():
    """Where the development PBX is. It is described on the machine that has
    one, never in this repository, so that publishing the tests publishes the
    procedure and not the network. KSIP_TEST_PBX names the folder under
    test-pbx/ (default local-asterisk)."""
    assert LAB.exists(), f'{LAB.relative_to(ROOT).as_posix()} がありません。開発用PBXの接続先を書いてください'
    settings = json.loads(LAB.read_text(encoding='utf-8-sig'))
    where = LAB.relative_to(ROOT).as_posix()
    for name in ('server', 'port', 'tls_host', 'tls_port', 'server_certificate', 'accounts'):
        assert name in settings, f'{where} に {name} がありません'
    for item in settings['accounts']:
        assert item.get('extension') and item.get('password'), f'{where} の accounts には extension と password が要ります'
    assert len(settings['accounts']) >= 2, f'{where} の accounts は2件以上要ります'
    return settings
def numbers():
    """The lab's dialplan conventions: what to dial for the long playback, the
    park slots and their park prefix, the ring group, voicemail and its direct
    prefix, and a number that does not exist."""
    return dict(DEFAULT_NUMBERS, **lab().get('numbers', {}))
def park_watch(slot):
    """What a BLF watches for that slot: the slot itself on Asterisk, or a
    lab.json "park_watch" template such as sip:park+{slot}@{server} for
    FreeSWITCH, whose valet parking publishes presence under park+<lot>."""
    return numbers().get('park_watch', '{slot}').format(slot=slot, server=lab()['server'])
def park_target(slot):
    """What a call is transferred to in order to park it on that slot."""
    return numbers()['park_prefix'] + slot
def ca_certificate():
    """The CA that issued the lab's TLS certificate, as lab.json names it
    (a path from the repository root; default the folder's LocalCA.crt)."""
    path = ROOT / lab().get('ca_certificate', f'test-pbx/{PBX}/LocalCA.crt')
    assert path.is_file(), f'{path.relative_to(ROOT).as_posix()} が必要です（TLSの検証に使う認証局の証明書）'
    return path
def feature(name):
    """Whether the lab's PBX provides a behaviour a test depends on; lab.json
    "features" lists the ones it lacks as false. Unknown names are provided."""
    return bool(lab().get('features', {}).get(name, True))
def skip_unless(name, reason):
    """Ends the test as skipped, and says why, when the PBX lacks a feature."""
    if not feature(name):
        print(f'SKIP: {reason}（{PBX} の lab.json で features.{name} が false）', flush=True)
        raise SystemExit(0)
ENCRYPTIONS = ('none', 'osrtp', 'sdes', 'dtls')
def accounts():
    """The test extensions of lab.json, in the order written there: the first
    two carry every test and the third the recording switch and the ring
    group, so those must take plain calls. Each carries `encryption`, which
    is how the PBX treats that extension: none, osrtp (keys offered on
    RTP/AVP, plain accepted), sdes (SRTP required) or dtls (DTLS-SRTP)."""
    where=lab()
    found=[dict(server=where['server'],port=where['port'],extension=item['extension'],auth_user=item['extension'],password=item['password'],
                encryption=item.get('encryption','none')) for item in where['accounts']]
    for a in found:
        assert a['encryption'] in ENCRYPTIONS, f"{LAB.relative_to(ROOT).as_posix()}: {a['extension']} の encryption は {', '.join(ENCRYPTIONS)} のどれかです"
        a['dtls']=a['encryption']=='dtls'
    for a in found[:3]:
        assert a['encryption'] in ('none','osrtp'), f"{LAB.relative_to(ROOT).as_posix()}: 先頭3件は平文で使うので encryption は none か osrtp にしてください（{a['extension']} は {a['encryption']}）"
    return found
def account(index, purpose):
    """The account at that position, or a clear word on what lab.json lacks."""
    configured=accounts()
    assert len(configured)>index, f'{LAB.relative_to(ROOT).as_posix()} の accounts に{index+1}件目がありません（{purpose}）'
    return configured[index]
def has_account(kind):
    """Whether the lab has an extension the PBX treats that way."""
    return any(a['encryption']==kind for a in accounts())
def account_with(kind, purpose):
    """The first extension the PBX treats that way, or a clear word on what lab.json lacks."""
    match=[a for a in accounts() if a['encryption']==kind]
    assert match, f'{LAB.relative_to(ROOT).as_posix()} の accounts に "encryption": "{kind}" の内線がありません（{purpose}）'
    return match[0]
def dtls_account():
    return account_with('dtls', 'DTLS-SRTPのテストに使う')
def free_ports(count=2, kind='udp'):
    """Ports nobody listens on right now, for a phone's SIP and RTP; the RTP one
    is followed by a gap, so a range of twenty stays free enough."""
    import random
    found=[]
    while len(found)<count:
        port=random.randrange(20000,60000,32)
        try:
            with socket.socket(socket.AF_INET,socket.SOCK_DGRAM if kind=='udp' else socket.SOCK_STREAM) as probe:
                probe.bind(('0.0.0.0',port))
        except OSError:
            continue
        found.append(port)
    return found
def put_account(target,account):
    """Only the sign-in secret goes to the vault; the address is engine config."""
    assert target.startswith('KSIP/Test/')
    data=account['password'].encode()
    blob=(C.c_ubyte*len(data)).from_buffer_copy(data)
    item=Credential(Type=1,TargetName=target,CredentialBlobSize=len(blob),CredentialBlob=blob,Persist=2,UserName=account['auth_user'])
    try:
        if not advapi.CredWriteW(C.byref(item),0): raise OSError(C.get_last_error(),'Test credential write failed')
    finally:C.memset(blob,0,len(blob))
def delete_account(target):
    assert target.startswith('KSIP/Test/')
    if not advapi.CredDeleteW(target,1,0) and C.get_last_error()!=1168: raise OSError(C.get_last_error(),'Test credential delete failed')

def connect(caller,callee,extension,known=()):
    """Place a call and answer it. Returns both sides' call ids."""
    call=caller.action('dial',value=extension)
    def fresh(state):
        return [c for c in state['calls'] if c['state']=='INCOMING' and c['id'] not in known]
    remote=fresh(callee.wait(lambda s:fresh(s)))[0]['id']
    callee.action('answer',remote)
    caller.wait(lambda s:any(c['id']==call and c['state']=='ESTABLISHED' for c in s['calls']))
    return call,remote
class Phone:
    def __init__(self,name,account,sip_port=None,rtp_port=None,audio_player='aufile,NUL',
                 audio_source=None,extra_config='',codecs=('g711','libg722','opus'),
                 transport='UDP',mediaenc='',ca_file=''):
        """codecs picks the loaded codec modules, so a narrowband peer can be simulated.

        transport/mediaenc/ca_file drive SIP over TLS and SRTP, the same keys the
        app writes into its own engine configuration.
        """
        import wave
        if sip_port is None or rtp_port is None:sip_port,rtp_port=free_ports()
        self.dir=ROOT/'temp/build/ksip-integration'/name;self.dir.mkdir(parents=True,exist_ok=True)
        self.account=account
        self.target='KSIP/Test/test-native-'+name
        self.responses={};self.events=[];self.cv=threading.Condition();self.serial=0
        put_account(self.target,account)
        with wave.open(str(self.dir/'silence.wav'),'wb') as w:
            w.setparams((1,2,48000,0,'NONE','not compressed'));w.writeframes(b'\0'*(48000*2*120))
        if audio_source is None:
            audio_source=f'aufile,{(self.dir/"silence.wav").as_posix()}'
        with socket.socket() as reserve:
            reserve.bind(('127.0.0.1',0));ctrl=reserve.getsockname()[1]
        codec_modules=chr(10).join(f'module {name}.dll' for name in codecs)
        if mediaenc:codec_modules+=chr(10)+'module srtp.dll'+chr(10)+'module dtls_srtp.dll'
        trust=f'sip_cafile {Path(ca_file).as_posix()}'+chr(10)+'sip_verify_server yes'+chr(10) if ca_file else ''
        # The file player writes stereo like a call recording; the real device path (ksip_audio) takes 48 kHz mono only.
        play_channels=2 if audio_player.startswith('aufile') else 1
        (self.dir/'config').write_text(f'''ksip_sip_server {account['server']}
ksip_sip_port {account['port']}
ksip_extension {account['extension']}
sip_listen 0.0.0.0:{sip_port}
ksip_sip_transport {transport}
ksip_mediaenc {mediaenc}
{trust}sip_transports {transport}
sip_cuser_random no
call_max_calls 8
call_hold_other_calls yes
audio_source {audio_source}
audio_player {audio_player}
audio_alert aufile,NUL
ausrc_srate 48000
auplay_srate 48000
ausrc_channels 1
auplay_channels {play_channels}
ausrc_format s16
auplay_format s16
auenc_format s16
audec_format s16
opus_stereo no
opus_sprop_stereo no
opus_bitrate 32000
opus_inbandfec yes
opus_packet_loss 10
opus_dtx no
opus_application voip
rtp_ports {rtp_port}-{rtp_port+20}
ctrl_tcp_listen 127.0.0.1:{ctrl}
{codec_modules}
module aufile.dll
module postlab.dll
module auconv.dll
module auresamp.dll
module ctrl_tcp.dll
module menu.dll
module ksip.dll
{extra_config}
''',encoding='utf-8')
        self.log=open(self.dir/'engine.log','wb')
        env=dict(os.environ,KSIP_CREDENTIAL_TARGET=self.target)
        engine=os.environ.get('KSIP_TEST_ENGINE_EXE')
        command=[engine,'--engine',str(os.getpid()),'-f',str(self.dir)] if engine else [str(ROOT/'temp/build/native/bin/baresip.exe'),'-f',str(self.dir)]
        self.proc=subprocess.Popen(command,cwd=ROOT/'temp/build/native/bin',env=env,stdin=subprocess.DEVNULL,stdout=self.log,stderr=subprocess.STDOUT,creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            deadline=time.monotonic()+10
            while True:
                try:self.sock=socket.create_connection(('127.0.0.1',ctrl),.3);break
                except OSError:
                    if self.proc.poll() is not None or time.monotonic()>deadline:raise RuntimeError('Engine startup failed')
                    time.sleep(.1)
            self.sock.settimeout(None)
            threading.Thread(target=self.read,daemon=True).start()
            self.command('ksip_login')
            self.wait(lambda s:s['registration']=='REGISTER_OK',timeout=20)
        except BaseException:self.close();raise
    def read(self):
        try:
            with self.sock.makefile('rb') as stream:
                while True:
                    header=b''
                    while True:
                        ch=stream.read(1)
                        if not ch:return
                        if ch==b':':break
                        header+=ch
                    value=json.loads(stream.read(int(header)));assert stream.read(1)==b','
                    with self.cv:
                        if value.get('event'):self.events.append(value)
                        if 'token' in value:self.responses[value['token']]=value
                        self.cv.notify_all()
        except OSError:pass
    def command(self,name,params=''):
        with self.cv:
            self.serial+=1;token=str(self.serial)
            data=json.dumps(dict(command=name,params=params,token=token)).encode()
            self.sock.sendall(str(len(data)).encode()+b':'+data+b',')
            end=time.monotonic()+8
            while token not in self.responses:
                remaining=end-time.monotonic()
                if remaining<=0:raise RuntimeError('Engine command timeout: '+name)
                self.cv.wait(remaining)
            result=self.responses.pop(token)
            if not result.get('ok'):raise RuntimeError('Engine rejected '+name+': '+result.get('data',''))
            return result.get('data','')
    def log_text(self):
        """What the engine has logged so far."""
        self.log.flush()
        return (self.dir/'engine.log').read_bytes().decode('utf-8','replace').replace(chr(13),chr(10))
    def action(self,op,id='',value=''):return self.command('ksip_action',json.dumps(dict(op=op,id=id,value=value))).strip()
    def state(self):return json.loads(self.command('ksip_state'))
    def wait(self,predicate,timeout=12):
        end=time.monotonic()+timeout
        while time.monotonic()<end:
            state=self.state()
            if predicate(state):return state
            time.sleep(.15)
        raise RuntimeError('Timed out waiting for SIP state; registration='+state['registration']+', calls='+str([(c['state'],c['held']) for c in state['calls']]))
    def close(self):
        if hasattr(self,'sock'):
            try:self.command('ksip_shutdown')
            except (OSError,RuntimeError):pass
            try:self.command('quit')
            except (OSError,RuntimeError):pass
        if hasattr(self,'proc'):
            try:self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:self.proc.kill();self.proc.wait()
        if hasattr(self,'sock'):self.sock.close()
        if hasattr(self,'log'):self.log.close()
        delete_account(self.target)
        log=self.dir/'engine.log'
        if log.exists() and self.account['password'].encode() in log.read_bytes():
            raise AssertionError('Secret leaked into the engine log of '+self.dir.name)
