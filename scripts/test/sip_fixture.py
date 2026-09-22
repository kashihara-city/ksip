"""Isolated KSIP test helpers. Never print credentials or write passwords to disk."""
from pathlib import Path
import ctypes as C
from ctypes import wintypes as W
import json, re, socket, subprocess, threading, time

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
def lab():
    """Where the development Asterisk is. It is described on the machine that
    has one, never in this repository, so that publishing the tests publishes
    the procedure and not the network."""
    path = ROOT / 'local-asterisk/lab.json'
    assert path.exists(), 'local-asterisk/lab.json がありません。開発用Asteriskの接続先を書いてください'
    settings = json.loads(path.read_text(encoding='utf-8-sig'))
    for name in ('server', 'port', 'tls_host', 'tls_port', 'server_certificate'):
        assert name in settings, f'local-asterisk/lab.json に {name} がありません'
    return settings
def accounts():
    """Every extension documented in local-asterisk, ordered by number."""
    text=(ROOT/'local-asterisk/asteriskserver.md').read_text(encoding='utf-8-sig')
    rows=sorted(re.findall(r'(?m)^(\d{3,6})[ \t]+([0-9a-fA-F]{32})[ \t]*$',text))
    assert len(rows)>=2, 'Expected at least two documented test accounts'
    where=lab()
    return [dict(server=where['server'],port=where['port'],extension=user,auth_user=user,password=password) for user,password in rows]
def account(extension):
    match=[a for a in accounts() if a['extension']==extension]
    assert match, f'{extension} is not documented in local-asterisk/asteriskserver.md'
    return match[0]
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
    def __init__(self,name,account,sip_port,rtp_port,audio_player='aufile,NUL',
                 audio_source=None,extra_config='',codecs=('g711','libg722','opus'),
                 transport='UDP',mediaenc='',ca_file=''):
        """codecs picks the loaded codec modules, so a narrowband peer can be simulated.

        transport/mediaenc/ca_file drive SIP over TLS and SRTP, the same keys the
        app writes into its own engine configuration.
        """
        import os, wave
        self.dir=ROOT/'temp/build/ksip-integration'/name;self.dir.mkdir(parents=True,exist_ok=True)
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
auplay_channels 1
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
