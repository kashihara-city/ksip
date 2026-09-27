// What the page knows about the phone right now (the last snapshot and the
// line the person is on), and every way of acting on it: run an operation,
// place a call, answer, hang up. The other modules read the state through the
// live bindings here and change it only through these functions.
import {$, invoke, logUi} from './ui.js';
import {t, language, languageFor, useLanguage} from './i18n.js';
import {render} from './phone.js';
import {kinds, deviceOptions} from './audio.js';
import {openSettings} from './settings.js';

export let state={running:false,calls:[],devices:[],transfer:{},account:{},settings:{}};
// The settings a policy fixes, by name (a button's five values each), from
// the app when the window starts: the same until KSIP starts again. The
// window offers no way to change them; a save brings them as they are, and
// the app refuses one that does not.
export const managed=new Set();
export const managedButton=n=>managed.has('button_'+n+'_kind');
export let ready=false,busy=false,selected=1,error='',first=true;
export function setError(text){error=text;}
export function setSelected(line){selected=line;}

export const callAt=n=>state.calls.find(c=>c.line===n);
export const current=()=>callAt(selected);
export const peerNumber=c=>c?.peer?.replace(/^sip:/,'').split('@')[0]||'';
// A URI may be written between angle brackets; the engine reports it bare.
export const address=text=>String(text||'').trim().replace(/^<\s*/,'').replace(/\s*>$/,'');
export const watchState=number=>state.parking?.find(slot=>slot.number===address(number))?.state||'UNKNOWN';
// The buttons a site defined, with their position, leaving out the unused ones.
export const configuredButtons=()=>(state.settings.buttons||[]).map((b,i)=>({...b,index:i+1})).filter(b=>b.kind);

let deviceKey='';
export function update(snapshot){
  // A saved language wins over the one Windows reports, and changing it in the
  // settings takes effect at once.
  const wanted=snapshot.settings?.language||navigator.language;
  if(languageFor(wanted)!==language)useLanguage(wanted);
  // When the call on the selected line has just ended and the other line still
  // has one, the controls follow it: the engine brings that call back, and the
  // buttons should be about it. A line chosen for a new call is left alone.
  const hadCall=!!callAt(selected);
  state=snapshot;ready=true;
  if(hadCall&&!callAt(selected)){const other=[1,2].find(n=>callAt(n));if(other)selected=other;}
  const key=JSON.stringify(state.devices);
  if(deviceKey!==key){for(const kind of kinds)deviceOptions(kind,state.settings[kind]||'default');deviceKey=key;}
  for(const kind of kinds)if($(kind).value!==state.settings[kind])$(kind).value=state.settings[kind];
  render();
  if(first){first=false;if(!state.account.has_password)openSettings();}
}
// An operation: the page is busy while it runs, the notices go, and the
// snapshot is read again once it is done.
export async function run(fn){
  if(busy)return;busy=true;error='';render({clearNotices:true});
  try{await fn();update(await invoke('snapshot'));}
  catch(e){error=String(e);logUi('command',e);throw e;}
  finally{busy=false;render();}
}
export async function act(name,id=current()?.id||'',value='',line=selected){
  try{await run(()=>invoke('action',{name,id,value,line}));}catch{}
}
// Every way of placing a call ends here: the dial box, a button, the history
// and a link. The selected line is used while it is free, otherwise the free
// one; the number goes into the dial box, so that what was dialled can be seen
// afterwards. The box and the buttons are disabled while something else is
// going on, so of the refusals only a link ever meets one.
export function dial(number){
  if(!number||busy||state.transfer.pending)return;
  if(state.registration!=='REGISTER_OK'){error=t('NOT_REGISTERED_YET');render();return;}
  const line=!callAt(selected)?selected:[1,2].find(n=>!callAt(n));
  if(!line){error=t('NO_FREE_LINE');render();return;}
  selected=line;$('target').value=number;render();return act('dial','',number,line);
}
// Answering goes to the line that rings, whichever is selected, which is how a
// link or a shortcut key finds it; the answer button is enabled only while the
// selected line rings, so for it the two are the same. With nothing to act on,
// the request is noted and nothing else happens.
export function answer(){
  const ringing=current()?.state==='INCOMING'?current():state.calls.find(c=>c.state==='INCOMING');
  if(!ringing){logUi('call','ANSWER without an incoming call');return;}
  selected=ringing.line;render();return act('answer',ringing.id,'',ringing.line);
}
export function hangup(){
  if(!current()){logUi('call','HANGUP without a call');return;}
  return act('hangup');
}
// A history row keeps the far end as the engine reported it: a SIP URI with
// the host, port and parameters of that call. Redialling someone on the
// account's own server dials the user part as a number, as the dial box
// would; anyone elsewhere, or a user part that is a name rather than a
// number, is dialled as the bare URI, so that the row and the call agree on
// who is being called. The port and parameters belonged to the old call.
export function dialHistory(peer){
  const text=address(peer);
  const match=/^(sips?):([^@;]+)(?:@([^;]+?))?(?::\d+)?(?:;.*)?$/i.exec(text);
  if(!match){dial(text);return;}
  const scheme=match[1].toLowerCase(),user=match[2],host=(match[3]||'').replace(/:\d+$/,'');
  const own=!host||host.toLowerCase()===String(state.account.server||'').toLowerCase();
  const number=/^\+?[0-9*#]+$/.test(user);
  dial(own&&number?user:host?`${scheme}:${user}@${host}`:user);
}
