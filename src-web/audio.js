// The microphone and the speaker: the device chosen for each, its Windows
// volume and mute, and the level meter that shows what is being heard.
import {$, invoke, logUi} from './ui.js';
import {t, fill} from './i18n.js';
import {state, ready, busy, run, update, setError} from './session.js';
import {render} from './phone.js';

export const kinds=['microphone','speaker'];
const peakPending={microphone:false,speaker:false};
export const volumes=Object.fromEntries(kinds.map(k=>[k,{pending:false,dragging:false,available:false,muted:false,sequence:0,queued:null}]));

export function deviceOptions(kind,selected){
  const options=[new Option(t(kind==='microphone'?'AUDIO_MICROPHONE_DEFAULT':'AUDIO_SPEAKER_DEFAULT'),'default')];
  for(const d of state.devices.filter(d=>d.kind===kind)) options.push(new Option(d.name,d.id));
  if(!options.some(o=>o.value===selected))options.push(new Option(t('AUDIO_DEVICE_SAVED_MISSING'),selected));
  $(kind).replaceChildren(...options);$(kind).value=selected;
}
function volumeDevice(kind){return state.running?state[kind+'_id']:state.settings[kind]||'default';}
// A device that is unplugged is written down when it goes and when it is
// back, not once a minute for as long as it is gone (a night of it was more
// than a thousand lines).
const unplugged={};
function logDevice(where,e){
  const gone=String(e)==='AUDIO_DEVICE_UNPLUGGED';
  if(!gone||!unplugged[where])logUi(where,e);
  unplugged[where]=gone;
}
function deviceWorks(where){
  if(unplugged[where])logUi(where,'device back');
  unplugged[where]=false;
}
// The endpoint the calls use for the choice: the chosen device, or the
// default communications one in its place while it is unplugged, the
// engine's own rule (device_selection.cc, alert_player.h). The volume and the
// meter show, and change, what is in use; `inPlace` says the default stands
// in. The default's own failure is written down apart, so that the chosen
// one's going and coming stays one line each.
async function onEndpoint(where,command,args){
  try{const result=await invoke(command,args);deviceWorks(where);return {result,inPlace:false};}
  catch(e){
    if(String(e)!=='AUDIO_DEVICE_UNPLUGGED'||args.device==='default')throw e;
    logDevice(where,e);
    try{return {result:await invoke(command,{...args,device:'default'}),inPlace:true};}
    catch(fallback){logUi(where+' (default)',fallback);throw {inPlace:fallback};}
  }
}
// The error to show and whether it is still to be written down.
function failure(where,e){
  if(e&&typeof e==='object'&&'inPlace' in e)return e.inPlace;
  logDevice(where,e);return e;
}
// The level and the mute are Windows' own for the device; either is set when
// given, and both are read back, so the icons follow what a headset key or
// another program did within a second. A change the person made always goes
// through the queue (queueChange), and leaves it only when it is sent: while
// nothing can be sent (a request on its way, the slider held, the phone busy
// with an operation) it waits there, and goes with the next read, which the
// end of the request, the end of the drag or the poll a second later brings.
async function refreshVolume(kind,level=null,mute=null){
  const control=volumes[kind];
  if(level!==null||mute!==null)queueChange(kind,{level,mute});
  if(!ready||busy||control.pending||control.dragging||control.pointer)return;
  const device=volumeDevice(kind),waiting=control.queued;
  // What waits for another device (it was switched meanwhile) is not sent.
  control.queued=null;
  if(waiting&&waiting.device===device){level=waiting.level;mute=waiting.mute;}
  const sequence=++control.sequence;control.pending=true;
  try{
    const {result,inPlace}=await onEndpoint('volume '+kind,'audio_volume',{kind,device,level,mute});
    if(sequence!==control.sequence||device!==volumeDevice(kind)||control.dragging)return;
    $(kind+'-volume').value=result.level;control.available=true;showMute(kind,result.muted);$(kind+'-level').textContent=result.level+'%';
    const notices=[];
    if(result.level>100)notices.push(fill('GAIN_APPLIED',result.level+'%'));
    if(inPlace)notices.push(t('AUDIO_DEVICE_FALLBACK'));
    if(kind==='microphone'&&state.microphone_fallback)notices.push(t('MICROPHONE_SILENT'));
    if(result.muted)notices.push(t('MICROPHONE_MUTED'));
    $(kind+'-volume-status').textContent=notices.join(' / ');$(kind+'-volume-status').classList.toggle('muted',result.muted);
  }
  catch(e){const error=failure('volume '+kind,e);control.available=false;$(kind+'-level').textContent='—';$(kind+'-volume-status').textContent=kind==='microphone'&&state.microphone_fallback?t('MICROPHONE_SILENT'):t(String(error));}
  finally{control.pending=false;render();if(control.queued)refreshVolume(kind);}
}
// A change waiting to be sent. The volume and the mute are kept apart: a
// change of one does not take back a waiting change of the other, so both
// are sent. A change for another device (the device was switched meanwhile)
// replaces what was waiting for the old one.
function queueChange(kind,change){
  const c=volumes[kind],device=volumeDevice(kind),waiting=c.queued&&c.queued.device===device?c.queued:{device,level:null,mute:null};
  ++c.sequence;
  c.queued={device,level:change.level??waiting.level,mute:change.mute??waiting.mute};
}
function showMute(kind,muted){
  const control=volumes[kind],button=$(kind+'-mute'),label=t(muted?'AUDIO_UNMUTE':'AUDIO_MUTE');
  control.muted=muted;button.classList.toggle('muted',muted);button.setAttribute('aria-pressed',String(muted));button.title=label;button.setAttribute('aria-label',label);
}
// Windows' privacy settings refusing the microphone: said under it, the
// switch that is off named, with a way to the settings; gone once the
// microphone opens again (the meter tries with each reading).
function showPrivacy(error){
  const refused=error.startsWith('MICROPHONE_PRIVACY');
  $('microphone-privacy').hidden=!refused;
  if(refused)$('microphone-privacy-text').textContent=t(error);
}
async function refreshPeak(kind){
  // In the tray nobody sees the meter, and not asking lets the microphone close.
  if(!ready||$('configuration').open||peakPending[kind]||state.window_visible===false)return;peakPending[kind]=true;
  try{
    const {result}=await onEndpoint('peak '+kind,'audio_peak',{kind,device:volumeDevice(kind)}),gain=kind==='microphone'?Math.max(1,(state.settings.microphone_gain||100)/100):1,raw=Math.max(0,Math.min(1,(result.peak||0)*gain)),db=raw>0?20*Math.log10(raw):-60,level=Math.round(Math.max(0,Math.min(100,(db+60)/60*100)));
    $(kind+'-meter-fill').style.width=level+'%';$(kind+'-meter').setAttribute('aria-valuenow',String(level));
    if(kind==='microphone')showPrivacy('');
  }
  catch(e){$(kind+'-meter-fill').style.width='0%';$(kind+'-meter').setAttribute('aria-valuenow','0');if(kind==='microphone')showPrivacy(String(e));failure('peak '+kind,e);}
  finally{peakPending[kind]=false;}
}
export async function pollVolumes(){await Promise.all(kinds.map(k=>refreshVolume(k)));setTimeout(pollVolumes,1000);}
export async function pollAudioPeaks(){await Promise.all(kinds.map(refreshPeak));setTimeout(pollAudioPeaks,100);}

export function init(){
  $('refresh-devices').addEventListener('click',async()=>{
    $('refresh-devices').disabled=true;$('device-status').textContent=t('DEVICES_LOADING');
    try{await invoke('refresh_devices');update(await invoke('snapshot'));$('device-status').textContent=fill('DEVICES_COUNT',state.devices.length);}
    catch(e){$('device-status').textContent=t(String(e));logUi('refresh devices',e);}
    finally{$('refresh-devices').disabled=false;}
  });
  $('open-microphone-privacy').addEventListener('click',()=>invoke('open_microphone_privacy').catch(e=>{setError(String(e));logUi('open microphone privacy',e);render();}));
  $('open-sound-control').addEventListener('click',()=>invoke('open_sound_control').catch(e=>{setError(String(e));logUi('open sound control',e);render();}));
  for(const kind of kinds)$(kind).addEventListener('change',async()=>{
    const previous=state.settings[kind],device=$(kind).value;
    // During a call the engine moves the call onto the device; otherwise a
    // device it does not take restarts it.
    $(kind+'-volume-status').textContent=t(state.calls.length?'AUDIO_SWITCHING':'RECONNECTING');
    try{await run(()=>invoke('select_audio_device',{kind,device}));}
    catch{$(kind).value=previous;}
  });
  for(const kind of kinds){
    $(kind+'-volume').addEventListener('pointerdown',()=>{volumes[kind].pointer=true;});
    for(const type of ['pointerup','pointercancel'])window.addEventListener(type,()=>{volumes[kind].pointer=false;});
    $(kind+'-volume').addEventListener('input',()=>{const slider=$(kind+'-volume');volumes[kind].dragging=true;if(volumes[kind].pointer&&Math.abs(Number(slider.value)-100)<=3)slider.value=100;$(kind+'-level').textContent=slider.value+'%';});
    $(kind+'-volume').addEventListener('change',()=>{const c=volumes[kind],level=Number($(kind+'-volume').value);c.dragging=false;c.pointer=false;refreshVolume(kind,level);});
    // A second click while one is waiting toggles the waiting one back.
    $(kind+'-mute').addEventListener('click',()=>{const c=volumes[kind],waiting=c.queued&&c.queued.device===volumeDevice(kind)?c.queued.mute:null;refreshVolume(kind,null,!(waiting??c.muted));});
  }
}
