"""Runs the settings dialog's own script with the page in headless Edge (Tauri's calls answered by the test) and checks that what the dialog shows and would save is what it was given, from a settings file and from the stored settings, never a value turned into another on the way."""
import argparse, http.server, json, os, shutil, subprocess, sys, tempfile, threading, winreg
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WEB = ROOT / 'src-web'
TYPES = {'.js': 'text/javascript', '.html': 'text/html', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.ico': 'image/x-icon'}

# The checks, run in the page as a module beside the app's own. The app's
# modules are the real ones; only window.__TAURI__ is the test's.
CHECKS = r"""
import {buildButtonSets} from './buttons.js';
import {init, openSettings} from './settings.js';
import {state} from './session.js';
const $=id=>document.getElementById(id);
const results=[];
const check=(ok,what,detail)=>results.push({ok:!!ok,what,detail:detail===undefined?'':JSON.stringify(detail)});
let answers={},saved=null;
window.__invoke=async(command,args)=>{
  if(command==='save_configuration'){saved=JSON.parse(JSON.stringify(args));throw 'TEST_SAVE_STOPPED';}
  if(command==='list_adapters')return [];
  if(command in answers)return answers[command];
  throw 'TEST_UNEXPECTED '+command;
};
const settle=()=>new Promise(r=>setTimeout(r,30));
const account={server:'pbx.example',port:5061,extension:'1001',auth_user:'1001',has_password:true};
const base={transport:'tls',media_encryption:'sdes',codecs:'opus',sip_port:5060,rtp_port:10000,register_interval:300,tray_after_call:-1,
  aec:true,aec_delay_ms:20,noise_suppression:'high',incoming_action:'show',language:'',microphone_gain:100,speaker_gain:100,buttons:[]};
async function open(settings={}){
  if($('configuration').open)$('configuration').close();
  state.settings={...base,...settings};state.account={...account};
  openSettings();await settle();
}
async function importFile(file){
  answers.import_settings_file={settings:{},buttons:[],account:{},unreadable:[],invalid:[],...file};
  $('import-settings').click();await settle();
}
async function save(){saved=null;$('settings-form').requestSubmit();await settle();return saved;}
const shown=id=>!$(id).hidden;
const text=id=>$(id).textContent;

async function checks(){
  buildButtonSets();init();

  await open();
  await importFile({settings:{transport:'tls',media_encryption:'dtls'}});
  let s=await save();
  check(s&&s.settings.transport==='tls'&&s.settings.media_encryption==='dtls','a file with TLS and DTLS is taken as it is',s&&s.settings.media_encryption);

  for(const [what,file] of [['UDP with SDES',{transport:'udp',media_encryption:'sdes'}],['UDP with OSRTP',{transport:'udp',media_encryption:'osrtp'}],['UDP alone, with SDES in the dialog',{transport:'udp'}],['TCP alone, with SDES in the dialog',{transport:'tcp'}]]){
    await open();
    await importFile({settings:{...file,sip_port:5070}});
    const refused=shown('settings-error')?text('settings-error'):'';
    s=await save();
    check(refused.includes('TLS')&&s&&s.settings.transport==='tls'&&s.settings.media_encryption==='sdes'&&s.settings.sip_port===5060,
      `a file with ${what} is refused whole, and the dialog stays as it was`,[refused,s&&s.settings.transport,s&&s.settings.media_encryption,s&&s.settings.sip_port]);
  }
  await open({media_encryption:'dtls'});
  await importFile({settings:{transport:'udp'}});
  s=await save();
  check(s&&s.settings.transport==='udp'&&s.settings.media_encryption==='dtls','a file with UDP alone, with DTLS in the dialog, is taken',s&&[s.settings.transport,s.settings.media_encryption]);
  await open({transport:'udp',media_encryption:''});
  await importFile({settings:{transport:'tls',media_encryption:'sdes'}});
  s=await save();
  check(s&&s.settings.transport==='tls'&&s.settings.media_encryption==='sdes','a file that moves the dialog from UDP to TLS with SDES is taken',s&&[s.settings.transport,s.settings.media_encryption]);

  await open();
  await importFile({settings:{media_encryption:'future-encryption'}});
  s=await save();
  const unknown=[...$('media_encryption').options].find(o=>o.value==='future-encryption');
  check(s&&s.settings.media_encryption==='future-encryption'&&unknown&&unknown.textContent.includes('future-encryption')&&unknown.textContent!=='future-encryption',
    'a media encryption the list lacks is shown as unknown and saved as it is (for the save to refuse), not as none',s&&s.settings.media_encryption);

  await open();
  await importFile({settings:{codecs:'PCMU,opus,G722'}});
  s=await save();
  check(s&&s.settings.codecs==='PCMU,opus,G722','the codecs a file names are ticked in its order',s&&s.settings.codecs);
  await open();
  await importFile({settings:{codecs:'pcmu, OpUs'}});
  s=await save();
  check(s&&s.settings.codecs==='PCMU,opus','codecs in lower or mixed case are the dialog own names',s&&s.settings.codecs);
  await open();
  await importFile({settings:{codecs:''}});
  s=await save();
  check(s&&s.settings.codecs==='opus,G722,PCMU,PCMA','a file with no codecs named ticks them all, as the app then offers all',s&&s.settings.codecs);
  await open({codecs:'pcmu'});
  s=await save();
  check(s&&s.settings.codecs==='PCMU','a stored codec in lower case opens ticked',s&&s.settings.codecs);
  await open({codecs:'PCMU,G729,pcmu'});
  const note=shown('settings-file-note')?text('settings-file-note'):'';
  s=await save();
  check(note.includes('G729')&&note.includes('pcmu')&&s&&s.settings.codecs==='PCMU','stored codecs the list cannot show are named on opening',[note,s&&s.settings.codecs]);
  await open();
  for(const box of $('codec-list').querySelectorAll('input'))box.checked=false;
  s=await save();
  check(s===null&&shown('settings-error'),'with no codec ticked the dialog is not saved (empty would offer them all)',text('settings-error'));

  await open({transport:'TLS',media_encryption:' SDES '});
  s=await save();
  check(s&&s.settings.transport==='tls'&&s.settings.media_encryption==='sdes','a stored transport and encryption in capitals open as they are',s&&[s.settings.transport,s.settings.media_encryption]);
  await open({transport:'udp',media_encryption:'sdes'});
  const stranded=shown('media-encryption-note')?text('media-encryption-note'):'';
  s=await save();
  check(s&&s.settings.transport==='udp'&&s.settings.media_encryption==='sdes'&&stranded.includes('TLS'),'stored UDP with SDES (from a policy) opens as it is, with the note that it needs TLS',[stranded,s&&s.settings.media_encryption]);
  await open();
  $('transport').value='udp';$('transport').dispatchEvent(new Event('change'));
  s=await save();
  check(s&&s.settings.media_encryption==='sdes'&&shown('media-encryption-note'),'changing the transport by hand leaves SDES chosen and says it needs TLS',s&&[s.settings.media_encryption,text('media-encryption-note')]);
  await open({language:'fr',incoming_action:'popup',noise_suppression:'extreme',buttons:[{kind:'call',title:'',number:'1001',transfer:'',pickup:''}]});
  s=await save();
  check(s&&s.settings.language==='fr'&&s.settings.incoming_action==='popup'&&s.settings.noise_suppression==='extreme'&&s.settings.buttons[0].kind==='call',
    'stored choices the lists lack open and save as they are',s&&[s.settings.language,s.settings.incoming_action,s.settings.noise_suppression,s.settings.buttons[0].kind]);
  await open({language:'en',incoming_action:'notify'});
  s=await save();
  const leftover=[...document.querySelectorAll('#settings-form option')].filter(o=>'unknown' in o.dataset).length;
  check(s&&s.settings.language==='en'&&s.settings.incoming_action==='notify'&&leftover===0,'opened again with known choices, no unknown option is left behind',leftover);

  await open();
  await importFile({settings:{sip_port:5070,aec:false,no_such_field:'x'},account:{server:'192.0.2.10'}});
  const notShown=shown('settings-error')?text('settings-error'):'';
  s=await save();
  check(notShown.includes('no_such_field')&&s&&s.settings.sip_port===5060&&s.settings.aec===true&&s.account.server==='pbx.example',
    'a file with an item the dialog cannot show is refused whole, and the dialog stays as it was',[notShown,s&&s.settings.sip_port,s&&s.settings.aec,s&&s.account.server]);
  await open();
  await importFile({settings:{agc:true},unreadable:['aec'],invalid:['codecs']});
  s=await save();
  check(text('settings-file-note').includes('aec')&&text('settings-file-note').includes('codecs')&&s&&s.settings.agc===true,'what a file could not bring is named',text('settings-file-note'));
  await open();
  await importFile({settings:{microphone_gain:150,auto_record:true,tray_after_call:30,ca_file:'C:\\ca\\pbx.pem'},
    buttons:[{n:2,kind:'park',title:'P',number:'701',transfer:'*701'}],account:{server:'192.0.2.10',port:5060,extension:'1002'}});
  s=await save();
  const b=s&&s.settings.buttons[1];
  check(s&&s.settings.microphone_gain===150&&s.settings.auto_record===true&&s.settings.tray_after_call===30&&s.settings.ca_file==='C:\\ca\\pbx.pem'&&b.kind==='park'&&b.number==='701'&&b.transfer==='*701'
    &&s.account.server==='192.0.2.10'&&s.account.port===5060&&s.account.extension==='1002','settings, a button, the account and values the dialog has no field for all come in',s&&[s.settings,s.account]);
}
try{await checks();}catch(e){check(false,'the checks ran to the end',String(e&&e.stack||e));}
await fetch('/result',{method:'POST',body:JSON.stringify(results)});
"""


def edge():
    """msedge.exe, which Windows 11 has: its App Paths entry, or where it installs."""
    for hive in (winreg.HKEY_LOCAL_MACHINE, winreg.HKEY_CURRENT_USER):
        try:
            with winreg.OpenKey(hive, r'SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\msedge.exe') as key:
                path = Path(winreg.QueryValue(key, None))
                if path.is_file():
                    return path
        except OSError:
            pass
    for base in (os.environ.get('ProgramFiles(x86)'), os.environ.get('ProgramFiles')):
        if base and (Path(base) / 'Microsoft/Edge/Application/msedge.exe').is_file():
            return Path(base) / 'Microsoft/Edge/Application/msedge.exe'
    raise SystemExit('msedge.exe not found')


def page():
    text = (WEB / 'index.html').read_text(encoding='utf-8')
    app = '<script type="module" src="app.js"></script>'
    assert text.count(app) == 1, 'index.html no longer loads app.js as the test expects'
    return text.replace(app, '<script>window.__TAURI__={core:{invoke:(c,a)=>window.__invoke(c,a)}};</script>'
                             '<script type="module" src="checks.js"></script>')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    received = {}
    done = threading.Event()

    class Handler(http.server.BaseHTTPRequestHandler):
        def send(self, body, kind):
            self.send_response(200)
            self.send_header('Content-Type', kind + '; charset=utf-8')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            path = self.path.split('?')[0]
            if path == '/':
                return self.send(page().encode('utf-8'), 'text/html')
            if path == '/checks.js':
                return self.send(CHECKS.encode('utf-8'), 'text/javascript')
            file = (WEB / path.lstrip('/')).resolve()
            if WEB.resolve() not in file.parents or not file.is_file():
                self.send_error(404)
                return
            self.send(file.read_bytes(), TYPES.get(file.suffix, 'application/octet-stream'))

        def do_POST(self):
            received['results'] = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            self.send(b'{}', 'application/json')
            done.set()

        def log_message(self, *args):
            pass

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    (ROOT / 'temp/build').mkdir(parents=True, exist_ok=True)
    profile = tempfile.mkdtemp(prefix='settings-dialog-edge-', dir=ROOT / 'temp/build')
    browser = subprocess.Popen([str(edge()), '--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
                                '--disable-extensions', f'--user-data-dir={profile}', f'http://127.0.0.1:{server.server_address[1]}/'],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        finished = done.wait(60)
    finally:
        browser.kill()
        browser.wait(10)
        server.shutdown()
        shutil.rmtree(profile, ignore_errors=True)
    if not finished:
        raise SystemExit('FAIL: the page sent no results within 60 s')
    results = received['results']
    (ROOT / 'temp/reports').mkdir(parents=True, exist_ok=True)
    (ROOT / 'temp/reports/settings-dialog.json').write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding='utf-8')
    failures = 0
    for r in results:
        print(('PASS: ' if r['ok'] else 'FAIL: ') + r['what'] + ('' if r['ok'] else f" ({r['detail']})"), flush=True)
        failures += not r['ok']
    if not results:
        failures += 1
    print(f"{'FAIL' if failures else 'PASS'}: {failures} failure(s) of {len(results)}")
    sys.exit(1 if failures else 0)


if __name__ == '__main__':
    main()
