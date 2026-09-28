"""Runs the settings dialog's own script with the page in headless Edge (Tauri's calls answered by the test) and checks that what the dialog shows and would save is what it was given, from a settings file and from the stored settings, never a value turned into another on the way."""
import argparse, http.server, json, os, shutil, subprocess, sys, tempfile, threading, winreg
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WEB = ROOT / 'src-web'
TYPES = {'.js': 'text/javascript', '.html': 'text/html', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.ico': 'image/x-icon'}

# The checks, run in the page as a module beside the app's own. The app's
# modules are the real ones; only window.__TAURI__ is the test's.
CHECKS = r"""
import {buildButtonSets, setEditing} from './buttons.js';
import * as buttons from './buttons.js';
import {init, openSettings, openButtonSettings} from './settings.js';
import {state, managed} from './session.js';
import {render, init as initPhone} from './phone.js';
const $=id=>document.getElementById(id);
const results=[];
const check=(ok,what,detail)=>results.push({ok:!!ok,what,detail:detail===undefined?'':JSON.stringify(detail)});
let answers={},saved=null,savedButtons=null,editingWindow=null,digitsSent=[];
window.__invoke=async(command,args)=>{
  // A digit's request takes a while, as one does when the app waits for the engine.
  if(command==='action'&&args.name==='dtmf'){digitsSent.push(args.value);await new Promise(r=>setTimeout(r,240));return '';}
  if(command==='save_configuration'){saved=JSON.parse(JSON.stringify(args));throw 'TEST_SAVE_STOPPED';}
  if(command==='save_buttons'){savedButtons=JSON.parse(JSON.stringify(args.buttons));throw 'TEST_SAVE_STOPPED';}
  if(command==='set_button_editing'){editingWindow=args.editing;return null;}
  if(command==='restart_settings')return ['network_adapter','sip_port','rtp_port','microphone','speaker','transport','ca_file','media_encryption','codecs','aec','aec_delay_ms','high_pass','noise_suppression','agc','register_interval','pbx_only'];
  if(command==='list_adapters')return [];
  if(command==='managed_settings')return [];
  if(command in answers)return answers[command];
  throw 'TEST_UNEXPECTED '+command;
};
const settle=()=>new Promise(r=>setTimeout(r,30));
const account={server:'pbx.example',port:5061,extension:'1001',auth_user:'1001',has_password:true};
// Every setting there, as the app's snapshot always has them.
const base={transport:'tls',media_encryption:'sdes',codecs:'opus',sip_port:5060,rtp_port:10000,register_interval:300,tray_after_call:-1,
  aec:true,aec_delay_ms:20,high_pass:false,noise_suppression:'high',agc:false,incoming_action:'show',language:'',microphone_gain:100,speaker_gain:100,
  network_adapter:'',ca_file:'',pbx_only:true,buttons:[]};
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
  buildButtonSets();init();initPhone();await settle();

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
  await open({browser_integration:true});
  const lockedWhileOff=$('browser_integration').disabled&&$('browser_dial_confirm').disabled;
  s=await save();
  check(lockedWhileOff&&s&&s.settings.program_integration===false&&s.settings.browser_integration===true,
    'with program links off the browser switches cannot be changed, and what they hold is saved as it is',s&&[lockedWhileOff,s.settings.program_integration,s.settings.browser_integration]);
  await open({program_integration:true,browser_integration:true});
  const openWhileOn=!$('browser_integration').disabled&&!$('browser_dial_confirm').disabled;
  $('program_integration').checked=false;$('program_integration').dispatchEvent(new Event('change'));
  const lockedAfter=$('browser_integration').disabled&&$('browser_integration').checked;
  check(openWhileOn&&lockedAfter,'program links on open the browser switches; turning them off locks the switches without clearing them',[openWhileOn,lockedAfter]);
  await open();
  s=await save();
  check(s&&s.settings.pbx_only===true,'with nothing stored, requests are taken from the registrar only',s&&s.settings.pbx_only);
  await open({pbx_only:false});
  s=await save();
  check(s&&s.settings.pbx_only===false,'a stored choice to take requests from anywhere is kept',s&&s.settings.pbx_only);

  await open();
  await importFile({settings:{microphone_gain:150,auto_record:true,tray_after_call:30,ca_file:'C:\\ca\\pbx.pem'},
    buttons:[{n:2,kind:'park',title:'P',number:'701',transfer:'*701'}],account:{server:'192.0.2.10',port:5060,extension:'1002'}});
  s=await save();
  const b=s&&s.settings.buttons[1];
  check(s&&s.settings.microphone_gain===150&&s.settings.auto_record===true&&s.settings.tray_after_call===30&&s.settings.ca_file==='C:\\ca\\pbx.pem'&&b.kind==='park'&&b.number==='701'&&b.transfer==='*701'
    &&s.account.server==='192.0.2.10'&&s.account.port===5060&&s.account.extension==='1002','settings, a button, the account and values the dialog has no field for all come in',s&&[s.settings,s.account]);

  // The save says whether it reconnects, from what was changed.
  const label=()=>$('save-settings').textContent;
  await open({codecs:''});
  const unchanged=label();
  $('language').value='en';$('language').dispatchEvent(new Event('change',{bubbles:true}));
  const windowOnly=label();
  $('sip_port').value='5070';$('sip_port').dispatchEvent(new Event('input',{bubbles:true}));
  const engine=label();
  check(unchanged==='保存'&&windowOnly==='保存'&&engine==='保存して再接続','the save says it reconnects only for a setting the engine reads at a start',[unchanged,windowOnly,engine]);
  s=await save();
  check(s&&s.settings.codecs==='','every codec ticked in the usual order stays the empty setting, so that saving it changes nothing',s&&s.settings.codecs);
  await open();
  $('password').value='new';$('password').dispatchEvent(new Event('input',{bubbles:true}));
  check(label()==='保存して再接続','a password typed in reconnects',label());

  // The dialog trimmed to one button: only its row is there, and saving saves the buttons alone.
  const visible=id=>{const e=$(id);return !!e&&e.getClientRects().length>0;};
  // One tab at a time, opening on the account; the save is outside the tabs.
  await open();
  const onOpen=[visible('server'),visible('aec'),visible('save-settings'),$('tab-account').getAttribute('aria-selected')];
  $('tab-audio').click();
  const onAudio=[visible('server'),visible('aec'),visible('save-settings'),$('tab-audio').getAttribute('aria-selected')];
  $('tab-audio').dispatchEvent(new KeyboardEvent('keydown',{key:'ArrowRight',bubbles:true}));
  const onNext=[visible('auto_answer'),visible('aec')];
  check(JSON.stringify(onOpen)==='[true,false,true,"true"]'&&JSON.stringify(onAudio)==='[false,true,true,"true"]'&&JSON.stringify(onNext)==='[true,false]',
    'the dialog opens on the account tab, shows one tab at a time, moves by arrow key, and keeps the save in sight',[onOpen,onAudio,onNext]);
  $('configuration').close();await settle();
  await open();
  check(visible('server'),'opened again, it starts on the account tab',visible('server'));
  if($('configuration').open)$('configuration').close();
  state.settings={...base,buttons:[{title:'A',kind:'dial',number:'1001',transfer:'',pickup:''},{title:'B',kind:'dial',number:'1002',transfer:'',pickup:''}]};state.account={...account};
  openButtonSettings(2);await settle();
  check(visible('button_2_kind')&&!visible('button_1_kind')&&!visible('button_8_kind')&&!visible('server')&&!visible('language')&&visible('close-settings')&&label()==='保存して継続',
    'trimmed to one button, the dialog shows that row, its close and "save and go on", and nothing else',[visible('button_2_kind'),visible('button_1_kind'),visible('server'),label()]);
  $('button_2_title').value='Bee';
  saved=null;savedButtons=null;$('settings-form').requestSubmit();await settle();
  check(saved===null&&savedButtons&&savedButtons.length===54&&savedButtons[1].title==='Bee'&&savedButtons[0].title==='A','the trimmed dialog saves the buttons alone',[saved,savedButtons&&savedButtons.slice(0,2)]);
  $('configuration').close();await settle();
  check(!$('configuration').classList.contains('trimmed')&&visible('button_1_kind')===false,'closed, the dialog is whole again for the next opening',$('configuration').className);

  // A call ringing leaves the dialog open; a call that starts closes it without saving, and says so.
  state.registration='REGISTER_OK';
  await open();
  state.calls=[{id:'c1',peer:'sip:1002@pbx.example',state:'INCOMING',held:false,duration:0,line:1}];render();
  const openWhileRinging=$('configuration').open;
  state.calls=[{id:'c1',peer:'sip:1002@pbx.example',state:'ESTABLISHED',held:false,duration:1,line:1}];saved=null;render();
  check(openWhileRinging&&!$('configuration').open&&saved===null&&text('error').includes('保存せずに閉じました'),'ringing leaves the settings open; a call that starts closes them unsaved and says so',[openWhileRinging,$('configuration').open,text('error')]);
  check($('settings-button').disabled&&$('edit-buttons').disabled,'in a call neither the settings nor the button editing can be entered',[$('settings-button').disabled,$('edit-buttons').disabled]);
  state.calls=[{id:'c1',peer:'sip:1002@pbx.example',state:'ESTABLISHED',held:true,duration:1,line:1}];render();
  check($('settings-button').disabled,'a call on hold counts as a call',$('settings-button').disabled);
  state.calls=[];render();

  // The connection switch shows one of its two sides.
  state.registration='REGISTER_OK';render();
  const whileRegistered=[$('unregister').hidden,$('reconnect').hidden];
  state.registration='REGISTER_FAIL';render();
  const whileFailed=[$('unregister').hidden,$('reconnect').hidden];
  check(!whileRegistered[0]&&whileRegistered[1]&&whileFailed[0]&&!whileFailed[1],'registered it offers to unregister, otherwise to connect again',[whileRegistered,whileFailed]);
  const markedAfterFailure=$('reconnect').classList.contains('on');
  state.registration='UNREGISTERED';render();
  check(!markedAfterFailure&&$('reconnect').classList.contains('on'),'the reconnect stands out after an unregister, not after a failure',[markedAfterFailure,$('reconnect').className]);
  state.registration='REGISTER_OK';render();

  // Button editing: every slot is shown with its tools, the rest of the phone is inert.
  state.settings={...base,buttons:[{title:'A',kind:'dial',number:'1001',transfer:'',pickup:''},{title:'B',kind:'park',number:'701',transfer:'*701',pickup:''}]};
  render();await settle();
  const size=id=>{const e=$(id),r=e.getBoundingClientRect(),style=getComputedStyle(e.querySelector('strong'));return [Math.round(r.height*10)/10,style.fontSize];};
  const before=size('custom-1');
  // Each button in the place of its slot: the empty slot before a set one
  // keeps its place, the rows after the last set one are left out.
  const places=box=>[...$(box).children].map(e=>e.id||'gap');
  const mainPlaces=places('custom-actions');
  state.settings={...state.settings,buttons:[{title:'A',kind:'dial',number:'1001',transfer:'',pickup:''},{},{title:'C',kind:'dial',number:'1003',transfer:'',pickup:''},{},{title:'E',kind:'dial',number:'1005',transfer:'',pickup:''}]};render();
  const secondRow=places('custom-actions');
  const panel=[{},{},{},{},{},{},{title:'P',kind:'dial',number:'1007',transfer:'',pickup:''},{},{},{},{},{},{title:'Q',kind:'dial',number:'1013',transfer:'',pickup:''}];
  state.settings={...state.settings,buttons:panel};render();
  const panelPlaces=places('extended-actions');
  // Beside the panel the phone keeps its own width, and the panel has two buttons to a row.
  const phoneWidth=document.querySelector('main').getBoundingClientRect().width,columns=getComputedStyle($('extended-actions')).gridTemplateColumns.split(' ').length;
  check(Math.round(phoneWidth)===Math.min(520,innerWidth)&&columns===2&&$('extended-actions-2').hidden,'beside the first panel the phone keeps its width, two to a row, no second panel',[phoneWidth,innerWidth,columns,$('extended-actions-2').hidden]);
  // A button from 31 on is in the second panel, beside the first, which stays
  // though it has none; each panel has as many columns.
  state.settings={...state.settings,buttons:[...Array(30).fill({}),{title:'R',kind:'dial',number:'1031',transfer:'',pickup:''}]};render();
  const secondPlaces=places('extended-actions-2'),firstShown=!$('extended-actions').hidden&&getComputedStyle($('extended-actions')).display==='grid',secondColumns=getComputedStyle($('extended-actions-2')).gridTemplateColumns.split(' ').length;
  check(JSON.stringify(secondPlaces)===JSON.stringify(['custom-31','gap'])&&firstShown&&$('extended-actions').children.length===0&&secondColumns===2&&document.body.classList.contains('extended-2'),
    'a button from 31 on is in the second panel, the first stays beside the phone, empty',[secondPlaces,firstShown,secondColumns]);
  state.settings={...state.settings,buttons:panel};render();
  // An empty slot is as tall as a button, so that a row of them does not close up.
  const gapHeight=$('extended-actions').querySelector('.button-gap').getBoundingClientRect().height,buttonHeight=$('custom-7').getBoundingClientRect().height;
  const thirteenTop=$('custom-13').getBoundingClientRect().top-$('custom-7').getBoundingClientRect().top;
  check(Math.abs(gapHeight-buttonHeight)<0.5&&thirteenTop>buttonHeight*3,'an empty slot is as tall as a button, so a button keeps its row',[gapHeight,buttonHeight,thirteenTop]);
  check(JSON.stringify(mainPlaces)===JSON.stringify(['custom-1','custom-2','gap'])
    &&JSON.stringify(secondRow)===JSON.stringify(['custom-1','gap','custom-3','gap','custom-5','gap'])
    &&JSON.stringify(panelPlaces)===JSON.stringify(['custom-7','gap','gap','gap','gap','gap','custom-13','gap']),
    'each button is in the place of its slot; the rows after the last set one are left out',[mainPlaces,secondRow,panelPlaces]);
  state.settings={...base,buttons:[{title:'A',kind:'dial',number:'1001',transfer:'',pickup:''},{title:'B',kind:'park',number:'701',transfer:'*701',pickup:''}]};render();await settle();
  setEditing(true);await settle();
  const after=size('custom-1'),second=$('custom-1').querySelector('small').textContent;
  check(JSON.stringify(before)===JSON.stringify(after)&&second==='1001'&&$('custom-1').textContent.includes('#1'),
    'a slot while editing has the height and font of the button itself, shows the number without the BLF state, and its place by the icons',[before,after,second]);
  const slots=document.querySelectorAll('.button-slot').length;
  check(slots===54&&editingWindow===true&&!!$('custom-2-delete')&&!$('custom-3-delete')&&!!$('custom-3-edit')&&!!$('dial').closest('[inert]')&&!$('custom-actions').closest('[inert]')&&$('edit-buttons').textContent==='ボタン編集終了',
    'editing shows all fifty-four slots with their tools, widens the window, and makes the rest of the phone inert',[slots,editingWindow,!!$('dial').closest('[inert]')]);
  // Deleting asks first, then saves the buttons without it.
  savedButtons=null;$('custom-2-delete').click();await settle();
  const asked=$('confirm').open;$('confirm-ok').click();await settle();
  check(asked&&savedButtons&&savedButtons[1].kind===''&&savedButtons[0].kind==='dial','deleting asks, then saves the buttons without that one',[asked,savedButtons&&savedButtons.slice(0,2)]);
  // Dropping onto another slot moves the button; onto a set one, the two change places.
  const drop=(from,to)=>{const data=new DataTransfer();data.setData('application/x-ksip-button',String(from));$('custom-'+to).dispatchEvent(new DragEvent('drop',{dataTransfer:data,bubbles:true,cancelable:true}));};
  savedButtons=null;drop(2,9);await settle();
  const moved=savedButtons;
  savedButtons=null;drop(1,2);await settle();
  const swapped=savedButtons;
  check(moved&&moved[8].title==='B'&&moved[1].kind===''&&swapped&&swapped[1].title==='A'&&swapped[0].title==='B','a button dropped on an empty slot moves there; on a set one, the two change places, and nothing else moves',[moved&&[moved[1].kind,moved[8].title],swapped&&[swapped[0].title,swapped[1].title]]);
  // Ringing keeps the editing on; a call that starts ends it.
  state.calls=[{id:'c1',peer:'sip:1002@pbx.example',state:'INCOMING',held:false,duration:0,line:1}];render();
  const stillEditing=buttons.editing;
  state.calls=[{id:'c1',peer:'sip:1002@pbx.example',state:'ESTABLISHED',held:false,duration:1,line:1}];render();
  check(stillEditing&&!buttons.editing&&editingWindow===false&&!$('dial').closest('[inert]'),'editing goes on while a call rings, and ends when one starts',[stillEditing,buttons.editing,editingWindow]);
  state.calls=[];render();

  // The DTMF method: shown as stored (none stored is RTP), saved as chosen,
  // and not a restart; a file brings it in the dialog's spelling.
  await open({dtmf_mode:'info'});
  const shownMode=$('dtmf_mode').value;
  $('dtmf_mode').value='inband';$('dtmf_mode').dispatchEvent(new Event('change',{bubbles:true}));
  const saveLabel=$('save-settings').textContent;
  s=await save();
  await open({});
  const noneStored=$('dtmf_mode').value;
  await importFile({settings:{dtmf_mode:'info'}});
  const imported=$('dtmf_mode').value;
  check(shownMode==='info'&&s&&s.settings.dtmf_mode==='inband'&&saveLabel==='保存'&&noneStored==='rtp'&&imported==='info','the DTMF method is shown, saved without a restart, RTP when none is stored, and taken from a file',[shownMode,s&&s.settings.dtmf_mode,saveLabel,noneStored,imported]);
  $('configuration').close();

  // Keys pressed while a digit is on its way are sent after it, in order; the
  // ones left when the call ends are not.
  state.calls=[{id:'c1',peer:'sip:1002@pbx.example',state:'ESTABLISHED',held:false,duration:1,line:1}];state.registration='REGISTER_OK';render();
  const keypad=digit=>[...$('keypad').children].find(b=>b.textContent===digit);
  digitsSent=[];
  for(const d of '123'){keypad(d).click();await new Promise(r=>setTimeout(r,60));}
  await new Promise(r=>setTimeout(r,900));
  const inOrder=[...digitsSent];
  digitsSent=[];
  for(const d of '456'){keypad(d).click();await new Promise(r=>setTimeout(r,20));}
  state.calls=[];render();
  await new Promise(r=>setTimeout(r,900));
  check(JSON.stringify(inOrder)==='["1","2","3"]'&&JSON.stringify(digitsSent)==='["4"]','keys pressed while a digit is on its way are all sent, in order; those left when the call ends are not',[inOrder,digitsSent]);

  // A button made unused is saved empty, whatever its number was.
  await open({buttons:[{title:'Web',kind:'open',number:'https://pbx.example/',transfer:'',pickup:''},{title:'',kind:'speed',number:'06-1234-5678',transfer:'',pickup:''}]});
  for(const n of [1,2]){$('button_'+n+'_kind').value='';$('button_'+n+'_kind').dispatchEvent(new Event('change',{bubbles:true}));}
  s=await save();
  const empty={title:'',kind:'',number:'',transfer:'',pickup:''};
  check(s&&JSON.stringify(s.settings.buttons.slice(0,2))===JSON.stringify([empty,empty]),'a button made unused is saved empty, its old link or number not sent',s&&s.settings.buttons.slice(0,2));

  // A settings file that changes what the engine reads at a start says so on the save at once.
  await open({transport:'tls',media_encryption:'sdes'});
  const beforeImport=$('save-settings').textContent;
  await importFile({settings:{transport:'tcp',media_encryption:''}});
  check(beforeImport==='保存'&&$('save-settings').textContent==='保存して再接続','a settings file that needs a reconnect says so on the save as soon as it is read',[beforeImport,$('save-settings').textContent]);
  $('configuration').close();

  // What a policy fixes: its field is locked and says so, whatever else the
  // dialog turns on; a save brings it as the app has it; a file cannot bring
  // it; a fixed button is not edited, moved or dropped on.
  if(buttons.editing)setEditing(false);
  for(const name of ['aec','browser_integration','server','codecs','sound_ring',...['title','kind','number','transfer','pickup'].map(f=>'button_3_'+f)])managed.add(name);
  const fixedButton={title:'C',kind:'speed',number:'1003',transfer:'',pickup:''};
  await open({program_integration:false,browser_integration:true,buttons:[{},{},fixedButton]});
  $('program_integration').checked=true;$('program_integration').dispatchEvent(new Event('change',{bubbles:true}));
  $('button_3_kind').value='park';$('button_3_kind').dispatchEvent(new Event('change',{bubbles:true}));
  const locked=['aec','browser_integration','server','sound_ring','button_3_kind','button_3_transfer'].filter(id=>!$(id).disabled);
  const codecLocked=[...$('codec-list').querySelectorAll('input,button')].every(e=>e.disabled);
  const chooser=$('sound_ring').closest('label').querySelector('button').disabled;
  check(!locked.length&&codecLocked&&chooser&&!$('browser_dial_confirm').disabled&&!$('port').disabled,'a fixed field stays locked when what it depends on turns on; the rest follow as before',[locked,codecLocked,chooser]);
  check($('aec').closest('label').dataset.managed==='管理者が設定'&&!!document.querySelector('#settings-buttons .button-index[data-managed]')&&!$('settings-managed-note').hidden,'a fixed field, a fixed button and the dialog say who decided it');
  $('aec').checked=false;$('server').value='192.0.2.10';$('language').value='en';
  s=await save();
  check(s&&s.settings.aec===true&&s.account.server==='pbx.example'&&s.settings.language==='en'&&s.settings.buttons[2].kind==='speed'&&s.settings.buttons[2].number==='1003',
    'a save brings what is fixed as the app has it, and the rest as the dialog holds it',s&&[s.settings.aec,s.account.server,s.settings.language,s.settings.buttons[2]]);
  await importFile({settings:{language:'ja'},managed:['aec','server']});
  check(text('settings-file-note').includes('aec, server'),'a file says what it left out because it is fixed',text('settings-file-note'));
  $('configuration').close();
  state.settings={...base,buttons:[{title:'A',kind:'dial',number:'1001',transfer:'',pickup:''},{},fixedButton]};render();
  setEditing(true);await settle();
  const fixedSlot=$('custom-3');
  savedButtons=null;
  const data=new DataTransfer();data.setData('application/x-ksip-button','1');fixedSlot.dispatchEvent(new DragEvent('drop',{dataTransfer:data,bubbles:true,cancelable:true}));await settle();
  check(!$('custom-3-edit')&&!$('custom-3-delete')&&!fixedSlot.draggable&&savedButtons===null&&!!$('custom-1-edit'),'a fixed button has no tools, is not dragged, and nothing is dropped on it',[!!$('custom-3-edit'),fixedSlot.draggable,savedButtons]);
  setEditing(false);
  // A fixed port stays as it is when the transport changes, shown and saved alike.
  managed.clear();managed.add('port');
  await open({transport:'tls'});
  $('transport').value='udp';$('transport').dispatchEvent(new Event('change',{bubbles:true}));
  const shownPort=$('port').value;
  s=await save();
  check(shownPort==='5061'&&s&&s.account.port===5061,'a fixed port is not changed with the transport, on screen or in the save',[shownPort,s&&s.account.port]);
  $('configuration').close();
  managed.add('auto_record');render();
  check($('record').disabled&&$('record').title==='管理者が設定','a fixed automatic recording is not switched on the phone, and says why',$('record').title);
  managed.clear();render();
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
