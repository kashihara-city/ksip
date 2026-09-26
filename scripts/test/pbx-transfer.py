"""REGISTER and two-leg attended REFER test against the lab PBX (KSIP_TEST_PBX)."""
import json,time
from sip_fixture import PBX,ROOT,Phone,accounts,connect,lab,version

def main():
    (ROOT/'temp/reports').mkdir(parents=True,exist_ok=True)
    configured=accounts();a=b=None
    try:
        a=Phone('a',configured[0],18560,18700)
        b=Phone('b',configured[1],18562,18800)
        print('PASS: both isolated endpoints REGISTER',flush=True)
        first,remote=connect(a,b,configured[1]['extension'])
        a.action('select')
        a.wait(lambda s:any(c['id']==first and c['held'] for c in s['calls']))
        second=a.action('dial',value=configured[1]['extension'])
        incoming=b.wait(lambda s:any(c['state']=='INCOMING' for c in s['calls']))
        remote2=next(c['id'] for c in incoming['calls'] if c['state']=='INCOMING')
        b.action('answer',remote2)
        a.wait(lambda s:len(s['calls'])==2 and all(c['state']=='ESTABLISHED' for c in s['calls']))
        for selected,held in [(first,second),(second,first)]:
            a.action('select',selected)
            a.wait(lambda s:any(c['id']==selected and not c['held'] for c in s['calls']) and any(c['id']==held and c['held'] for c in s['calls']))
        print('PASS: two calls established; switching holds the other leg',flush=True)
        # Recording belongs to a call, not the last created/destroyed audio stream.
        a.action('select',first)
        b.action('select',remote)
        time.sleep(.5)
        recording=a.dir/'first-call.wav'
        a.command('lab_record',first+' '+str(recording))
        time.sleep(.5)
        b.action('hangup',remote2)
        a.wait(lambda s:len(s['calls'])==1)
        header=recording.read_bytes()
        assert len(header)>=44 and header[40:44]==b'\0\0\0\0', 'Closing the other call ended this recording'
        a.command('lab_stop')
        import wave
        with wave.open(str(recording),'rb') as wav:assert wav.getnframes()>0
        second=a.action('dial',value=configured[1]['extension'])
        incoming=b.wait(lambda s:any(c['state']=='INCOMING' for c in s['calls']))
        remote2=next(c['id'] for c in incoming['calls'] if c['state']=='INCOMING')
        b.action('answer',remote2)
        a.wait(lambda s:len(s['calls'])==2 and all(c['state']=='ESTABLISHED' for c in s['calls']))
        a.action('transfer',first,second)
        result=a.wait(lambda s:not s['calls'] and s['transfer']['outcome']=='TRANSFER_DONE',timeout=25)
        b.wait(lambda s:len(s['calls'])==2)
        print('PASS: attended transfer completed on the PBX; both local legs ended',flush=True)
        report={'registration':True,'twoEstablishedCalls':True,'switchHoldsOther':True,'recordingSurvivesOtherCallClosing':True,'attendedTransferCompleted':True,'localLegsClosed':True,'server':'{0}:{1}'.format(lab()['server'],lab()['port'])}
        (ROOT/f'temp/reports/pbx-transfer-{PBX}-v{version()}.json').write_text(json.dumps(report,indent=2)+'\n')
    finally:
        if a:a.close()
        if b:b.close()

if __name__=='__main__':main()
