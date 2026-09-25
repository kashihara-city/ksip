"""Two real baresip processes, loopback SIP/RTP, no audio hardware needed.
A sends silence; B sends a tone. RX recording must contain only the peer.
"""
from pathlib import Path
import array, json, math, socket, subprocess, threading, time, wave

ROOT=Path(__file__).resolve().parents[2]
BASE=ROOT/'temp/build/call-test'
BASE.mkdir(parents=True,exist_ok=True)
(ROOT/'temp/reports').mkdir(parents=True,exist_ok=True)

def source(path,tone):
    samples=array.array('h',(int(8000*math.sin(2*math.pi*440*i/48000)) if tone else 0 for i in range(48000*30)))
    with wave.open(str(path),'wb') as f:
        f.setparams((1,2,48000,0,'NONE','not compressed')); f.writeframes(samples.tobytes())

class Phone:
    def __init__(self,name,sip,ctrl,rtp,aec,tone,discard_playback=False):
        self.name=name;self.events=[];self.responses={};self.cv=threading.Condition();self.serial=0
        self.dir=BASE/name;self.dir.mkdir(exist_ok=True)
        source(self.dir/'source.wav',tone)
        config=f'''sip_listen 0.0.0.0:{sip}
net_interface 127.0.0.1
sip_transports udp
sip_cuser_random no
call_max_calls 1
audio_source aufile,{(self.dir/'source.wav').as_posix()}
audio_player aufile,{'NUL' if discard_playback else (self.dir/'speaker.wav').as_posix()}
audio_alert aufile,{'NUL' if discard_playback else (self.dir/'alert.wav').as_posix()}
ausrc_srate 48000
auplay_srate 48000
ausrc_channels 1
auplay_channels 2
ausrc_format s16
auplay_format s16
auenc_format s16
audec_format s16
rtp_ports {rtp}-{rtp+20}
ctrl_tcp_listen 127.0.0.1:{ctrl}
module g711.dll
module aufile.dll
module postlab.dll
module auconv.dll
module auresamp.dll
module ctrl_tcp.dll
module menu.dll
module_app account.dll
'''
        (self.dir/'config').write_text(config,encoding='utf-8')
        (self.dir/'accounts').write_text(f'<sip:{name}@localhost:{sip};transport=udp>;regint=0;audio_codecs=PCMU/8000/1;answermode=manual\n',encoding='utf-8')
        self.log=open(self.dir/'engine.log','wb')
        self.proc=subprocess.Popen([str(ROOT/'temp/build/native/bin/baresip.exe'),'-f',str(self.dir)],cwd=self.dir,stdin=subprocess.DEVNULL,stdout=self.log,stderr=subprocess.STDOUT,creationflags=subprocess.CREATE_NO_WINDOW)
        try:
            end=time.monotonic()+10
            while True:
                try:self.sock=socket.create_connection(('127.0.0.1',ctrl),.3);break
                except OSError:
                    if self.proc.poll() is not None or time.monotonic()>end:raise RuntimeError(f'{name} startup failed; see {self.dir}/engine.log')
                    time.sleep(.1)
            self.sock.settimeout(None)
            threading.Thread(target=self.read,daemon=True).start()
        except BaseException:
            self.proc.kill();self.proc.wait();self.log.close();raise
    def read(self):
        try:
            with self.sock.makefile('rb') as f:
                while True:
                    h=b''
                    while True:
                        b=f.read(1)
                        if not b:return
                        if b==b':':break
                        h+=b
                    v=json.loads(f.read(int(h)));assert f.read(1)==b','
                    with self.cv:
                        if v.get('event'):self.events.append(v)
                        if 'token' in v:self.responses[v['token']]=v
                        self.cv.notify_all()
        except OSError:pass
    def command(self,cmd,params=''):
        self.serial+=1;token=str(self.serial)
        data=json.dumps(dict(command=cmd,params=params,token=token)).encode()
        self.sock.sendall(str(len(data)).encode()+b':'+data+b',')
        with self.cv:
            assert self.cv.wait_for(lambda:token in self.responses,8),f'{self.name}: timeout {cmd}'
            v=self.responses.pop(token)
        assert v.get('ok'),v
        return v
    def event(self,kind):
        with self.cv:
            assert self.cv.wait_for(lambda:any(e.get('type')==kind for e in self.events),10),(self.name,kind,self.events)
    def close(self):
        try:
            if self.proc.poll() is None:self.command('quit')
        except (OSError,AssertionError):pass
        try:self.proc.wait(timeout=4)
        except subprocess.TimeoutExpired:self.proc.kill();self.proc.wait()
        self.sock.close();self.log.close()

def stats(path):
    # Stereo like a call recording (far end on the left); the player fixture
    # is set to two channels as well, so the first channel is read either way.
    with wave.open(str(path),'rb') as f:
        assert f.getnchannels() in (1,2) and f.getsampwidth()==2
        rate=f.getframerate();s=array.array('h',f.readframes(f.getnframes()))[0::f.getnchannels()]
    assert len(s)>rate*2,(path,len(s),rate)
    return dict(rate=rate,samples=len(s),rms=math.sqrt(sum(x*x for x in s)/len(s)))

def main():
    phones=[]
    try:
        a=Phone('a',15060,15444,16000,True,False);phones.append(a)
        b=Phone('b',15062,15445,16100,False,True);phones.append(b)
        b.command('dial','sip:a@127.0.0.1:15060');a.event('CALL_INCOMING');a.command('accept')
        a.event('CALL_ESTABLISHED');b.event('CALL_ESTABLISHED')
        time.sleep(1)
        for p in phones:p.command('lab_record',str(p.dir/'receive.wav'))
        time.sleep(4)
        for p in phones:p.command('lab_stop')
        b.command('hangup');a.event('CALL_CLOSED');b.event('CALL_CLOSED')
    finally:
        for p in reversed(phones):p.close()
    result={p.name:stats(p.dir/'receive.wav') for p in phones}
    assert result['a']['rms']>1000,result
    assert result['b']['rms']<30,result # B's own microphone tone must NOT be in B's recording.
    log=(a.dir/'engine.log').read_text(encoding='utf-8',errors='replace')
    assert '48000 Hz, 1 channels' in log,log
    (ROOT/'temp/reports/call-test.json').write_text(json.dumps(dict(passed=True,results=result,scope='Deterministic loopback SIP/RTP and RX-only WAV using virtual file audio. WebRTC device/APM is covered separately.'),indent=2),encoding='utf-8')
    print(json.dumps(result,indent=2));print('PASS: two-process SIP/RTP and RX-only WAV at 48 kHz')
if __name__=='__main__':main()
