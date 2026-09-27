// The settings dialog: what it shows when opened, how its parts depend on
// each other, the echo calibration, and what it saves.
import {$, invoke, logUi} from './ui.js';
import {t, fill} from './i18n.js';
import {state, busy, run} from './session.js';
import {BUTTON_MAIN, BUTTON_INDEXES, defaultTitle} from './buttons.js';

// The codecs the setting can name, in the order offered when it names none.
const CODEC_NAMES=['opus','G722','PCMU','PCMA'];
// Settings the dialog has no field for (the gains and the automatic
// recording are set on the phone itself): a value an imported file brings
// is carried here until the dialog is saved or closed.
let carried={};

async function fillAdapters(selected){
  // The list is read when the dialog opens: what is plugged in can change.
  const select=$('network_adapter');
  select.textContent='';
  const add=(value,text)=>{const option=document.createElement('option');option.value=value;option.textContent=text;select.append(option);};
  add('',t('ADAPTER_AUTOMATIC'));
  let adapters=[];
  try{adapters=await invoke('list_adapters');}catch(e){logUi('list adapters',e);}
  for(const adapter of adapters)add(adapter.name,adapter.label+' — '+(adapter.address||t(adapter.up?'ADAPTER_NO_IP':'ADAPTER_DISCONNECTED')));
  // A saved adapter that is not there any more stays visible, so that saving
  // the dialog cannot quietly move the phone to another adapter.
  if(selected&&!adapters.some(adapter=>adapter.name===selected))add(selected,fill('ADAPTER_MISSING',selected));
  select.value=selected;
}
// The list shows the given order first, ticked, then the rest unticked in the usual order.
function setCodecs(codecs){
  const codecOrder=(codecs||'').split(',').map(s=>s.trim()).filter(Boolean),codecsInUse=codecOrder.length?codecOrder:CODEC_NAMES,codecList=$('codec-list');
  for(const name of [...codecsInUse,...CODEC_NAMES.filter(n=>!codecsInUse.includes(n))]){const row=codecList.querySelector(`[data-codec="${name}"]`);if(row){codecList.appendChild(row);row.querySelector('input').checked=codecsInUse.includes(name);}}
}
export function openSettings(){
  carried={};$('settings-file-note').hidden=true;
  for(const key of ['server','port','extension','auth_user'])$(key).value=state.account[key]??'';
  for(const key of ['sip_port','rtp_port'])$(key).value=state.settings[key];
  for(const n of BUTTON_INDEXES){const b=(state.settings.buttons||[])[n-1]||{};$('button_'+n+'_title').value=b.title||'';$('button_'+n+'_kind').value=b.kind||'';$('button_'+n+'_number').value=b.number||'';$('button_'+n+'_transfer').value=b.transfer||'';$('button_'+n+'_pickup').value=b.pickup||'';syncButtonRow(n);}
  fillAdapters(state.settings.network_adapter||'');
  for(const key of ['sound_ring', 'sound_ringback', 'sound_busy', 'sound_notfound', 'sound_error'])$(key).value=state.settings[key]||'';
  setCodecs(state.settings.codecs);
  $('password').value='';$('register_interval').value=state.settings.register_interval??300;$('detail_log').checked=!!state.settings.detail_log;
  $('shortcut_window').value=state.settings.shortcut_window||'';$('shortcut_call').value=state.settings.shortcut_call||'';
  $('incoming_action').value=state.settings.incoming_action==='notify'?'notify':'show';$('tray_after_call').value=state.settings.tray_after_call??-1;
  $('language').value=state.settings.language||'';$('browser_integration').checked=!!state.settings.browser_integration;$('browser_dial_confirm').checked=state.settings.browser_dial_confirm!==false;
  $('transport').value=state.settings.transport||'udp';$('media_encryption').value=state.settings.media_encryption||'';syncEncryptionChoices();$('ca_file').value=state.settings.ca_file||'';
  $('auto_answer').checked=!!state.settings.auto_answer;
  $('aec').checked=state.settings.aec;$('aec_delay_ms').value=state.settings.aec_delay_ms??20;$('high_pass').checked=!!state.settings.high_pass;$('noise_suppression').value=state.settings.noise_suppression||'high';$('agc').checked=!!state.settings.agc;
  $('calibration-status').textContent=t('SETTINGS_CALIBRATION_IDLE');
  $('password-hint').textContent=t(state.account.has_password?'SETTINGS_PASSWORD_SAVED':'SETTINGS_PASSWORD_HINT');
  $('settings-error').hidden=true;$('configuration').showModal();
}
// SDES and OSRTP carry their keys in the signalling, so they are offered only
// while the signalling is TLS; the note under the choice says so.
function syncEncryptionChoices(){
  const tls=$('transport').value==='tls',media=$('media_encryption');
  for(const option of media.options){if(option.value==='sdes'||option.value==='osrtp'){option.hidden=!tls;option.disabled=!tls;}}
  if(!tls&&(media.value==='sdes'||media.value==='osrtp'))media.value='';
  const note=$('media-encryption-note');note.textContent=tls?'':t('SETTINGS_MEDIA_TLS_ONLY');note.hidden=tls;
}
// The transfer target only means something for a park button.
// Only the fields a kind uses are open; the rest are greyed out, so that
// what a button does can be read off the settings.
function syncButtonRow(n){
  const kind=$('button_'+n+'_kind').value,watches=kind==='dial'||kind==='park';
  $('button_'+n+'_title').disabled=!kind;$('button_'+n+'_title').placeholder=defaultTitle(kind);$('button_'+n+'_number').disabled=!kind||kind==='dnd';
  $('button_'+n+'_transfer').disabled=kind!=='park';$('button_'+n+'_pickup').disabled=!watches;
  const set=side=>BUTTON_INDEXES.filter(k=>(k<=BUTTON_MAIN)===side&&$('button_'+k+'_kind').value).length;
  $('button-count').textContent=set(true)?fill('BUTTON_COUNT',set(true)):'';
  $('extended-count').textContent=set(false)?fill('BUTTON_COUNT',set(false)):'';
}
async function calibrateAec(careful){
  $('calibration-status').textContent=t(careful?'CALIBRATION_RUNNING_CAREFUL':'CALIBRATION_RUNNING_SIMPLE');
  try{await run(async()=>{const result=await invoke('calibrate_aec',{microphone:state.settings.microphone||'default',speaker:state.settings.speaker||'default',careful});$('aec_delay_ms').value=result.recommended_ms;$('calibration-status').textContent=fill('CALIBRATION_RESULT',t(result.stable?'CALIBRATION_RECOMMENDED':'CALIBRATION_UNSTABLE'),result.recommended_ms,result.samples,result.spread_ms,result.confidence);});}
  catch(e){$('calibration-status').textContent=t(String(e));logUi('aec calibration',e);}
}
// A settings file's values (as import_settings_file hands them over) go
// into the dialog; what the file leaves out stays as the dialog has it.
// Nothing is saved until the dialog is.
const SWITCHES=['auto_answer','aec','high_pass','agc','detail_log','browser_integration','browser_dial_confirm'];
const CARRIED=['microphone_gain','speaker_gain','auto_record'];
function applyImport(file){
  const settings=file.settings||{};
  // The transport first: the media encryption offers SDES and OSRTP only under TLS.
  if('transport' in settings){$('transport').value=settings.transport||'udp';syncEncryptionChoices();}
  for(const [key,value] of Object.entries(settings)){
    if(key==='transport'||key==='media_encryption')continue;
    if(SWITCHES.includes(key))$(key).checked=!!value;
    else if(CARRIED.includes(key))carried[key]=value;
    else if(key==='codecs')setCodecs(value);
    else if($(key))$(key).value=value;
  }
  if('media_encryption' in settings){$('media_encryption').value=settings.media_encryption;syncEncryptionChoices();}
  for(const button of file.buttons||[]){
    const n=button.n;if(!BUTTON_INDEXES.includes(n))continue;
    for(const field of ['title','kind','number','transfer','pickup'])if(field in button)$('button_'+n+'_'+field).value=button[field];
    syncButtonRow(n);
  }
  for(const key of ['server','port','extension'])if(key in (file.account||{}))$(key).value=file.account[key];
  const passedOver=[...(file.unreadable||[]),...(file.invalid||[])];
  const note=$('settings-file-note');
  note.textContent=t('SETTINGS_IMPORTED')+(passedOver.length?' '+fill('SETTINGS_IMPORT_PASSED_OVER',passedOver.join(', ')):'');
  note.hidden=false;
}
function showFileError(what,e){$('settings-error').textContent=t(String(e));$('settings-error').hidden=false;logUi(what,e);}
// Everything the dialog holds, as the app stores it.
function collectSettings(){
  return {
    network_adapter:$('network_adapter').value,sip_port:Number($('sip_port').value),rtp_port:Number($('rtp_port').value),
    microphone:state.settings.microphone,speaker:state.settings.speaker,
    microphone_gain:carried.microphone_gain??(state.settings.microphone_gain||100),speaker_gain:carried.speaker_gain??(state.settings.speaker_gain||100),
    auto_record:carried.auto_record??!!state.settings.auto_record,
    buttons:BUTTON_INDEXES.map(n=>({title:$('button_'+n+'_title').value.trim(),kind:$('button_'+n+'_kind').value,number:$('button_'+n+'_kind').value==='dnd'?'':$('button_'+n+'_number').value.trim(),transfer:$('button_'+n+'_kind').value==='park'?$('button_'+n+'_transfer').value.trim():'',pickup:['dial','park'].includes($('button_'+n+'_kind').value)?$('button_'+n+'_pickup').value.trim():''})),
    auto_answer:$('auto_answer').checked,
    aec:$('aec').checked,aec_delay_ms:Number($('aec_delay_ms').value),high_pass:$('high_pass').checked,noise_suppression:$('noise_suppression').value,agc:$('agc').checked,
    register_interval:Number($('register_interval').value),detail_log:$('detail_log').checked,
    shortcut_window:$('shortcut_window').value.trim(),shortcut_call:$('shortcut_call').value.trim(),
    incoming_action:$('incoming_action').value,tray_after_call:Number($('tray_after_call').value),language:$('language').value,
    browser_integration:$('browser_integration').checked,browser_dial_confirm:$('browser_dial_confirm').checked,
    transport:$('transport').value,media_encryption:$('media_encryption').value,
    codecs:[...$('codec-list').querySelectorAll('li')].filter(li=>li.querySelector('input').checked).map(li=>li.dataset.codec).join(','),
    ca_file:$('ca_file').value.trim(),
    sound_ring:$('sound_ring').value.trim(),sound_ringback:$('sound_ringback').value.trim(),sound_busy:$('sound_busy').value.trim(),sound_notfound:$('sound_notfound').value.trim(),sound_error:$('sound_error').value.trim(),
  };
}
export function init(){
  $('settings-button').addEventListener('click',openSettings);
  $('close-settings').addEventListener('click',()=>{$('password').value='';carried={};$('configuration').close();});
  // A codec moves one row up or down; the rows' order is the order offered.
  $('codec-list').addEventListener('click',e=>{const button=e.target.closest('button[data-move]');if(!button)return;const row=button.closest('li'),list=row.parentElement;if(button.dataset.move==='-1'){if(row.previousElementSibling)list.insertBefore(row,row.previousElementSibling);}else if(row.nextElementSibling)list.insertBefore(row.nextElementSibling,row);});
  $('configuration').addEventListener('cancel',e=>{if(busy)e.preventDefault();else $('password').value='';});
  $('transport').addEventListener('change',()=>{
    // 既定のポートを使っているときだけ、方式に合わせて入れ替える。
    const tls=$('transport').value==='tls',port=$('port');
    if(port.value.trim()===(tls?'5060':'5061'))port.value=tls?'5061':'5060';
    syncEncryptionChoices();
  });
  for(const n of BUTTON_INDEXES)$('button_'+n+'_kind').addEventListener('change',()=>syncButtonRow(n));
  $('choose-ca').addEventListener('click',async()=>{
    try{const chosen=await invoke('choose_sound_file',{kind:'certificate'});if(chosen)$('ca_file').value=chosen;}
    catch(e){$('settings-error').textContent=t(String(e));$('settings-error').hidden=false;logUi('choose ca file',e);}
  });
  for(const button of document.querySelectorAll('[data-sound]'))button.addEventListener('click',async()=>{
    try{const chosen=await invoke('choose_sound_file');if(chosen)$('sound_'+button.dataset.sound).value=chosen;}
    catch(e){$('settings-error').textContent=t(String(e));$('settings-error').hidden=false;logUi('choose sound file',e);}
  });
  $('import-settings').addEventListener('click',async()=>{
    $('settings-error').hidden=true;
    try{const file=await invoke('import_settings_file');if(file)applyImport(file);}
    catch(e){showFileError('import settings',e);}
  });
  $('export-settings').addEventListener('click',async()=>{
    $('settings-error').hidden=true;
    try{const path=await invoke('export_settings_file');if(path){const note=$('settings-file-note');note.textContent=fill('SETTINGS_EXPORTED',path);note.hidden=false;}}
    catch(e){showFileError('export settings',e);}
  });
  $('calibrate-aec').addEventListener('click',()=>calibrateAec(false));
  $('calibrate-aec-careful').addEventListener('click',()=>calibrateAec(true));
  $('settings-form').addEventListener('submit',async e=>{
    e.preventDefault();$('settings-error').hidden=true;
    const settings=collectSettings();
    const account={server:$('server').value.trim(),port:Number($('port').value),extension:$('extension').value.trim(),auth_user:$('auth_user').value.trim(),password:$('password').value};
    try{await run(()=>invoke('save_configuration',{settings,account}));$('password').value='';$('configuration').close();}
    catch(e){$('settings-error').textContent=t(String(e));$('settings-error').hidden=false;}
    finally{account.password='';}
  });
}
