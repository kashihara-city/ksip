// The microphone and the speaker: the device chosen for each, its Windows
// volume and mute, and the level meter that shows what is being heard.
//
// Which endpoint the volume, the mute and the meter go to is the backend's
// to say (commands.rs, target): the one the call is on while a call is up,
// as the engine reports it, and what the saved choice stands for otherwise,
// the default in place of a device that is not there. The page asks by kind
// alone, and each answer names the endpoint (id) and whether it stands in
// for the device chosen (stand_in: 'absent' for one that is not there,
// 'failed' for one the call could not use).
import {$, invoke, logUi} from './ui.js';
import {t, fill} from './i18n.js';
import {state, ready, busy, run, update, setError} from './session.js';
import {render} from './phone.js';

export const kinds=['microphone','speaker'];
const peakPending={microphone:false,speaker:false};
// For each kind: a request on its way, the slider held, what the last answer
// said (available, muted, the endpoint's id), and a change waiting to be sent.
export const volumes=Object.fromEntries(kinds.map(k=>[k,{pending:false,dragging:false,available:false,muted:false,id:null,sequence:0,queued:null}]));

export function deviceOptions(kind,selected){
  const options=[new Option(t(kind==='microphone'?'AUDIO_MICROPHONE_DEFAULT':'AUDIO_SPEAKER_DEFAULT'),'default')];
  for(const d of state.devices.filter(d=>d.kind===kind)) options.push(new Option(d.name,d.id));
  if(!options.some(o=>o.value===selected))options.push(new Option(t('AUDIO_DEVICE_SAVED_MISSING'),selected));
  $(kind).replaceChildren(...options);$(kind).value=selected;
}
// A failure is written down when it comes and when it changes, not once a
// second for as long as it lasts; the backend writes down a device going and
// coming back itself.
const said={};
function logFailure(where,e){
  const text=String(e);
  if(said[where]===text)return;
  said[where]=text;logUi(where,e);
}
function logWorks(where){
  if(said[where])logUi(where,'ok');
  delete said[where];
}
// The level and the mute are Windows' own for the endpoint in use; either is
// set when given, and both are read back, so the icons follow what a headset
// key or another program did within a second. A change the person made
// always goes through the queue (queueChange), and leaves it only when it is
// sent: while nothing can be sent (a request on its way, the slider held,
// the phone busy with an operation) it waits there, and goes with the next
// read, which the end of the request, the end of the drag or the poll a
// second later brings. It is sent for the endpoint it was meant for
// (`expected`): the backend leaves it unmade when another is in use by then.
async function refreshVolume(kind,level=null,mute=null){
  const control=volumes[kind];
  if(level!==null||mute!==null)queueChange(kind,{level,mute});
  if(!ready||busy||control.pending||control.dragging||control.pointer)return;
  const waiting=control.queued;
  control.queued=null;
  const expected=waiting?waiting.device:control.id;
  if(waiting){level=waiting.level;mute=waiting.mute;}
  const sequence=++control.sequence;control.pending=true;
  try{
    const result=await invoke('audio_volume',{kind,level,mute,expected});
    if(sequence!==control.sequence||control.dragging)return;
    control.id=result.id;
    $(kind+'-volume').value=result.level;control.available=true;showMute(kind,result.muted);$(kind+'-level').textContent=result.level+'%';
    const notices=[];
    if(result.level>100)notices.push(fill('GAIN_APPLIED',result.level+'%'));
    if(result.stand_in==='absent')notices.push(t('AUDIO_DEVICE_FALLBACK'));
    if(result.stand_in==='failed')notices.push(t('AUDIO_DEVICE_KEPT'));
    if(kind==='microphone'&&state.microphone_fallback)notices.push(t('MICROPHONE_SILENT'));
    if(result.muted)notices.push(t('MICROPHONE_MUTED'));
    $(kind+'-volume-status').textContent=notices.join(' / ');$(kind+'-volume-status').classList.toggle('muted',result.muted);
    logWorks('volume '+kind);
  }
  catch(e){logFailure('volume '+kind,e);control.id=null;control.available=false;$(kind+'-level').textContent='—';$(kind+'-volume-status').textContent=kind==='microphone'&&state.microphone_fallback?t('MICROPHONE_SILENT'):t(String(e));}
  finally{control.pending=false;render();if(control.queued)refreshVolume(kind);}
}
// A change waiting to be sent, for the endpoint shown when it was made. The
// volume and the mute are kept apart: a change of one does not take back a
// waiting change of the other, so both are sent. A change for another
// endpoint (the call moved meanwhile) replaces what was waiting for the old one.
function queueChange(kind,change){
  const c=volumes[kind],device=c.id,waiting=c.queued&&c.queued.device===device?c.queued:{device,level:null,mute:null};
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
    const result=await invoke('audio_peak',{kind}),gain=kind==='microphone'?Math.max(1,(state.settings.microphone_gain||100)/100):1,raw=Math.max(0,Math.min(1,(result.peak||0)*gain)),db=raw>0?20*Math.log10(raw):-60,level=Math.round(Math.max(0,Math.min(100,(db+60)/60*100)));
    $(kind+'-meter-fill').style.width=level+'%';$(kind+'-meter').setAttribute('aria-valuenow',String(level));
    if(kind==='microphone')showPrivacy('');
    logWorks('peak '+kind);
  }
  catch(e){$(kind+'-meter-fill').style.width='0%';$(kind+'-meter').setAttribute('aria-valuenow','0');if(kind==='microphone')showPrivacy(String(e));logFailure('peak '+kind,e);}
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
    $(kind+'-mute').addEventListener('click',()=>{const c=volumes[kind],waiting=c.queued&&c.queued.device===c.id?c.queued.mute:null;refreshVolume(kind,null,!(waiting??c.muted));});
  }
}
