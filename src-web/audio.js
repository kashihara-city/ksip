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
// For each kind: a request on its way, the slider held, what the last answer
// said (available, muted, the endpoint's id), and a change waiting to be sent.
export const volumes=Object.fromEntries(kinds.map(k=>[k,{pending:false,dragging:false,available:false,muted:false,id:null,sequence:0,queued:null}]));

// The devices of a kind to choose from, in the main screen's list of the
// kind or another (`select`), after the choices that list has of its own.
export function deviceOptions(kind,selected,select=$(kind),leading=[]){
  const options=[...leading,new Option(t(kind==='microphone'?'AUDIO_MICROPHONE_DEFAULT':'AUDIO_SPEAKER_DEFAULT'),'default')];
  for(const d of state.devices.filter(d=>d.kind===kind)) options.push(new Option(d.name,d.id));
  if(!options.some(o=>o.value===selected))options.push(new Option(t('AUDIO_DEVICE_SAVED_MISSING'),selected));
  select.replaceChildren(...options);select.value=selected;
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
// The level and the mute are Windows' own for the endpoint in use; both are
// read with every look (pollAudio), so the icons follow what a headset key
// or another program did within a tenth of a second, and either is set when
// the person asks. A change the person made always goes through the queue
// (queueChange), and leaves it only when it is sent (sendVolume): while
// nothing can be sent (a request on its way, the slider held, the phone busy
// with an operation) it waits there, and goes with the next look. It is sent
// for the endpoint it was meant for (`expected`): the backend leaves it
// unmade when another is in use by then.
async function sendVolume(kind){
  const control=volumes[kind];
  if(!ready||busy||control.pending||control.dragging||control.pointer||!control.queued)return;
  const waiting=control.queued;control.queued=null;
  const sequence=++control.sequence;control.pending=true;
  try{
    const result=await invoke('audio_volume',{kind,level:waiting.level,mute:waiting.mute,expected:waiting.device});
    if(sequence!==control.sequence||control.dragging)return;
    applyVolume(kind,result);
  }
  catch(e){failVolume(kind,e);}
  finally{control.pending=false;render();if(control.queued)sendVolume(kind);}
}
// The level over the slider; above 100, the words for the software gain
// before it (null: no level to show).
function showLevel(kind,level){
  $(kind+'-level').textContent=level===null?'—':level+'%';
  $(kind+'-gain').hidden=!(level>100);
}
// What a look, or a change, said about a kind of device, onto its controls.
function applyVolume(kind,result){
  const control=volumes[kind];
  control.id=result.id;
  $(kind+'-volume').value=result.level;control.available=true;showMute(kind,result.muted);showLevel(kind,result.level);
  const notices=[];
  if(result.stand_in==='absent')notices.push(t('AUDIO_DEVICE_FALLBACK'));
  if(result.stand_in==='failed')notices.push(t('AUDIO_DEVICE_KEPT'));
  if(kind==='microphone'&&state.microphone_fallback)notices.push(t('MICROPHONE_SILENT'));
  if(result.muted)notices.push(t('MICROPHONE_MUTED'));
  $(kind+'-volume-status').textContent=notices.join(' / ');$(kind+'-volume-status').classList.toggle('muted',result.muted);$(kind+'-volume-status').classList.remove('missing');
  logWorks('volume '+kind);
}
function failVolume(kind,e){
  const control=volumes[kind];
  logFailure('volume '+kind,e);control.id=null;control.available=false;showLevel(kind,null);
  const none=String(e)==='AUDIO_MICROPHONE_NONE'||String(e)==='AUDIO_SPEAKER_NONE';
  $(kind+'-volume-status').textContent=kind==='microphone'&&state.microphone_fallback&&!none?t('MICROPHONE_SILENT'):t(String(e));
  $(kind+'-volume-status').classList.toggle('missing',none);
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

// The buttons beside the level move it a percent at a time, and on and on
// while one is held: a step at the press, then one every REPEAT_MS once it
// has been held HOLD_MS. The level shows as it moves, as when the slider is
// dragged, and is sent when the button is let go, since a gain is saved
// with every change sent.
const HOLD_MS=400,REPEAT_MS=50;
let stepping=null;
function stepLevel(kind,delta){
  const slider=$(kind+'-volume'),level=Math.max(0,Math.min(Number(slider.max),Number(slider.value)+delta));
  slider.value=level;showLevel(kind,level);
}
function sendLevel(kind){
  volumes[kind].dragging=false;
  queueChange(kind,{level:Number($(kind+'-volume').value),mute:null});sendVolume(kind);
}
function startStepping(kind,button,delta){
  stopStepping();
  volumes[kind].dragging=true;stepLevel(kind,delta);
  // A button disabled while held (the phone busy) hears no letting go.
  const tick=()=>{if(button.disabled){stopStepping();return;}stepLevel(kind,delta);stepping.timer=setTimeout(tick,REPEAT_MS);};
  stepping={kind,timer:setTimeout(tick,HOLD_MS)};
}
function stopStepping(){
  if(!stepping)return;
  const {kind,timer}=stepping;stepping=null;clearTimeout(timer);
  sendLevel(kind);
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

// The meter: the peak off the endpoint in use, with the saved gain laid over,
// as a level in dB over sixty.
function applyPeak(kind,peak,error){
  if(error){$(kind+'-meter-fill').style.width='0%';$(kind+'-meter').setAttribute('aria-valuenow','0');if(kind==='microphone')showPrivacy(String(error));logFailure('peak '+kind,error);return;}
  const gain=kind==='microphone'?Math.max(1,(state.settings.microphone_gain||100)/100):1,raw=Math.max(0,Math.min(1,(peak||0)*gain)),db=raw>0?20*Math.log10(raw):-60,level=Math.round(Math.max(0,Math.min(100,(db+60)/60*100)));
  $(kind+'-meter-fill').style.width=level+'%';$(kind+'-meter').setAttribute('aria-valuenow',String(level));
  if(kind==='microphone')showPrivacy('');
  logWorks('peak '+kind);
}
// The meters are wanted while the window is shown and the settings are not
// over it; nobody sees them otherwise, and not asking lets the microphone
// close.
function metersWanted(){return ready&&state.window_visible!==false&&!$('configuration').open;}
// One look for both kinds (audio_levels): the volumes always, so that the
// device chosen going and coming is written down through the night, the
// meters when wanted. A change waiting to be sent goes first. The backend
// looks at the device lists along the way, so a device plugged in or pulled
// shows in the lists within a second. One look at a time; a look is skipped
// while the phone is busy with an operation.
let looking=false;
export async function look(){
  if(!ready||busy||looking)return;
  const meters=metersWanted();
  looking=true;
  try{
    for(const kind of kinds)if(volumes[kind].queued)await sendVolume(kind);
    const sequences=Object.fromEntries(kinds.map(k=>[k,++volumes[k].sequence]));
    const levels=await invoke('audio_levels',{meters});
    for(const kind of kinds){
      const control=volumes[kind],side=levels[kind];
      if(sequences[kind]!==control.sequence||control.pending||control.dragging||control.pointer)continue;
      if(side.volume)applyVolume(kind,side.volume);else failVolume(kind,side.volume_error);
      if(meters)applyPeak(kind,side.peak,side.peak_error);
    }
  }
  catch(e){logUi('audio levels',e);}
  finally{looking=false;render();}
}
// The looks, ten times a second while the meters are wanted, once a second
// in the tray and behind the settings.
export async function pollAudio(){
  await look();
  setTimeout(pollAudio,metersWanted()?100:1000);
}

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
    $(kind+'-volume').addEventListener('input',()=>{const slider=$(kind+'-volume');volumes[kind].dragging=true;if(volumes[kind].pointer&&Math.abs(Number(slider.value)-100)<=3)slider.value=100;showLevel(kind,Number(slider.value));});
    $(kind+'-volume').addEventListener('change',()=>{const c=volumes[kind],level=Number($(kind+'-volume').value);c.dragging=false;c.pointer=false;queueChange(kind,{level,mute:null});sendVolume(kind);});
    for(const [suffix,delta] of [['-volume-down',-1],['-volume-up',1]]){
      const button=$(kind+suffix);
      // The click that ends a press of the pointer has had its steps; one
      // with no press before it (the keyboard, UI Automation) is one step,
      // sent at once.
      let pressed=false;
      button.addEventListener('pointerdown',e=>{
        if(e.button!==0||button.disabled)return;
        pressed=true;
        // Held on when the pointer strays off it; one the page cannot capture still steps.
        try{button.setPointerCapture(e.pointerId);}catch{}
        startStepping(kind,button,delta);
      });
      for(const type of ['pointerup','pointercancel','lostpointercapture'])button.addEventListener(type,stopStepping);
      button.addEventListener('pointerup',()=>setTimeout(()=>{pressed=false;}));
      button.addEventListener('pointercancel',()=>{pressed=false;});
      button.addEventListener('click',()=>{
        if(pressed){pressed=false;return;}
        if(!button.disabled){stepLevel(kind,delta);sendLevel(kind);}
      });
    }
    // A second click while one is waiting toggles the waiting one back.
    $(kind+'-mute').addEventListener('click',()=>{const c=volumes[kind],waiting=c.queued&&c.queued.device===c.id?c.queued.mute:null;queueChange(kind,{level:null,mute:!(waiting??c.muted)});sendVolume(kind);});
  }
}
