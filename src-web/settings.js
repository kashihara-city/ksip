// The settings dialog: what it shows when opened, how its parts depend on
// each other, the echo calibration, and what it saves.
import {$, invoke, logUi} from './ui.js';
import {t, fill} from './i18n.js';
import {state, busy, run, setError} from './session.js';
import {BUTTON_MAIN, BUTTON_INDEXES, defaultTitle} from './buttons.js';

// The codecs the setting can name, in the order offered when it names none.
const CODEC_NAMES=['opus','G722','PCMU','PCMA'];
// Settings the dialog has no field for (the gains and the automatic
// recording are set on the phone itself): a value an imported file brings
// is carried here until the dialog is saved or closed.
let carried={};
// The settings a save needs the engine started again for (from the app),
// and the button the dialog is trimmed to, 0 when it shows everything.
let restartNames=[];
let trimmedTo=0;

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
// The list shows the given order first, ticked, then the rest unticked in the
// usual order; none given ticks them all, as the app then offers them all. A
// name is matched whatever its case, as the app reads it. The names the list
// cannot show (ones this version does not have, or one given twice) are
// returned, for the dialog to say that saving drops them.
function codecNames(codecs){
  const given=String(codecs??'').split(',').map(s=>s.trim()).filter(Boolean),order=[],dropped=[];
  for(const name of given){const known=CODEC_NAMES.find(n=>n.toLowerCase()===name.toLowerCase());if(known&&!order.includes(known))order.push(known);else dropped.push(name);}
  return {inUse:given.length?order:CODEC_NAMES,dropped};
}
function setCodecs(codecs){
  const {inUse,dropped}=codecNames(codecs),codecList=$('codec-list');
  for(const name of [...inUse,...CODEC_NAMES.filter(n=>!inUse.includes(n))]){const row=codecList.querySelector(`[data-codec="${name}"]`);if(row){codecList.appendChild(row);row.querySelector('input').checked=inUse.includes(name);}}
  return dropped;
}
// The ticked codecs in their order, as the setting names them.
const tickedCodecs=()=>[...$('codec-list').querySelectorAll('li')].filter(li=>li.querySelector('input').checked).map(li=>li.dataset.codec).join(',');
// A choice of a list. A value the list does not have is shown as an option
// of its own, marked as unknown, rather than as nothing: nothing means
// something for some of these (no encryption, UDP), and the dialog would
// save it. Saved as it is, the save's own check then refuses what this
// version does not know.
function setChoice(select,value){
  const text=String(value??'');
  for(const option of [...select.options])if('unknown' in option.dataset&&option.value!==text)option.remove();
  if(![...select.options].some(option=>option.value===text)){const option=document.createElement('option');option.value=text;option.textContent=fill('SETTINGS_CHOICE_UNKNOWN',text);option.dataset.unknown='';select.append(option);}
  select.value=text;
}
// The dialog shows one tab at a time; it opens on the account, where a first
// setup begins. The footer (the error and the save) is outside the tabs, so
// a refusal is seen whichever tab is open.
export function selectTab(name){
  for(const tab of document.querySelectorAll('.settings-tabs [role="tab"]')){
    const on=tab.dataset.tab===name;
    tab.setAttribute('aria-selected',String(on));tab.tabIndex=on?0:-1;
    $('settings-'+tab.dataset.tab).hidden=!on;
  }
}
// The dialog trimmed to one button, from the button editing: the title with
// its close, that button's row, and a save that keeps the phone as it is.
// Saving it saves the buttons alone.
export function openButtonSettings(n){
  openSettings();
  trimmedTo=n;
  const dialog=$('configuration'),row=$('button_'+n+'_kind').closest('.button-set');
  dialog.classList.add('trimmed');selectTab('buttons');
  for(const set of dialog.querySelectorAll('.button-set'))set.classList.toggle('kept',set===row);
  updateSaveLabel();
}
// Back to the whole dialog, whenever it closes.
function untrim(){
  trimmedTo=0;
  $('configuration').classList.remove('trimmed');
  for(const set of document.querySelectorAll('.button-set.kept'))set.classList.remove('kept');
}
// A call has started (by any way: a key, the automatic answer, a link):
// the dialog closes without saving, trimmed or not, and says so.
export function closeSettingsForCall(){
  const dialog=$('configuration');
  if(!dialog.open)return false;
  $('password').value='';carried={};
  dialog.close();
  return true;
}
// What the account fields hold, as the save sends them.
function collectAccount(){
  return {server:$('server').value.trim(),port:Number($('port').value),extension:$('extension').value.trim(),auth_user:$('auth_user').value.trim(),password:$('password').value};
}
// Whether saving what the dialog holds starts the engine again: another
// account, a password typed, or a change to a setting the engine reads only
// at a start. The app decides the same way when it saves.
function needsRestart(){
  const a=collectAccount(),known=state.account,s=collectSettings();
  const accountChanged=!!a.password||!known.has_password||a.server!==(known.server??'')||a.port!==known.port||a.extension!==(known.extension??'')||a.auth_user!==(known.auth_user??'');
  return accountChanged||restartNames.some(name=>JSON.stringify(s[name])!==JSON.stringify(state.settings[name]));
}
function updateSaveLabel(){
  $('save-settings').textContent=t(trimmedTo?'BUTTON_SAVE_CONTINUE':needsRestart()?'SETTINGS_SAVE_RECONNECT':'SETTINGS_SAVE_ONLY');
}
export function openSettings(){
  carried={};$('settings-file-note').hidden=true;
  for(const key of ['server','port','extension','auth_user'])$(key).value=state.account[key]??'';
  for(const key of ['sip_port','rtp_port'])$(key).value=state.settings[key];
  for(const n of BUTTON_INDEXES){const b=(state.settings.buttons||[])[n-1]||{};$('button_'+n+'_title').value=b.title||'';setChoice($('button_'+n+'_kind'),b.kind||'');$('button_'+n+'_number').value=b.number||'';$('button_'+n+'_transfer').value=b.transfer||'';$('button_'+n+'_pickup').value=b.pickup||'';syncButtonRow(n);}
  fillAdapters(state.settings.network_adapter||'');
  for(const key of ['sound_ring', 'sound_ringback', 'sound_busy', 'sound_notfound', 'sound_error'])$(key).value=state.settings[key]||'';
  const dropped=setCodecs(state.settings.codecs);
  $('password').value='';$('register_interval').value=state.settings.register_interval??300;$('detail_log').checked=!!state.settings.detail_log;
  $('shortcut_window').value=state.settings.shortcut_window||'';$('shortcut_call').value=state.settings.shortcut_call||'';
  setChoice($('incoming_action'),state.settings.incoming_action||'show');$('tray_after_call').value=state.settings.tray_after_call??-1;
  setChoice($('language'),state.settings.language||'');$('program_integration').checked=!!state.settings.program_integration;$('browser_integration').checked=!!state.settings.browser_integration;$('browser_dial_confirm').checked=state.settings.browser_dial_confirm!==false;
  // The app reads these two whatever their case and spaces; the list has them in lower case.
  $('pbx_only').checked=state.settings.pbx_only!==false;syncIntegration();
  setChoice($('transport'),String(state.settings.transport||'').trim().toLowerCase()||'udp');setChoice($('media_encryption'),String(state.settings.media_encryption||'').trim().toLowerCase());syncEncryptionChoices();$('ca_file').value=state.settings.ca_file||'';
  $('auto_answer').checked=!!state.settings.auto_answer;
  $('aec').checked=state.settings.aec;$('aec_delay_ms').value=state.settings.aec_delay_ms??20;$('high_pass').checked=!!state.settings.high_pass;setChoice($('noise_suppression'),state.settings.noise_suppression||'high');$('agc').checked=!!state.settings.agc;
  $('calibration-status').textContent=t('SETTINGS_CALIBRATION_IDLE');
  $('password-hint').textContent=t(state.account.has_password?'SETTINGS_PASSWORD_SAVED':'SETTINGS_PASSWORD_HINT');
  if(dropped.length){const note=$('settings-file-note');note.textContent=fill('SETTINGS_CODECS_DROPPED',dropped.join(', '));note.hidden=false;}
  $('settings-error').hidden=true;selectTab('account');$('configuration').showModal();
  updateSaveLabel();
}
// SDES and OSRTP carry their keys in the signalling, so they are offered only
// while the signalling is TLS; the note under the choice says so. One already
// chosen stays chosen when the transport changes: the note then says it needs
// TLS and the save refuses it, so that the encryption is never turned off
// without someone choosing that.
function syncEncryptionChoices(){
  const tls=$('transport').value==='tls',media=$('media_encryption');
  for(const option of media.options){if(option.value==='sdes'||option.value==='osrtp'){option.hidden=!tls&&!option.selected;option.disabled=!tls;}}
  const stranded=!tls&&(media.value==='sdes'||media.value==='osrtp');
  const note=$('media-encryption-note');note.textContent=tls?'':t(stranded?'SETTINGS_SDES_NEEDS_TLS':'SETTINGS_MEDIA_TLS_ONLY');note.hidden=tls;
}
// The browser is one of the programs that operate the phone: its switches
// can be turned only while program links are on. What they hold is kept
// either way (it is saved as it is), and has no effect while program links
// are off.
function syncIntegration(){
  const on=$('program_integration').checked;
  for(const id of ['browser_integration','browser_dial_confirm'])$(id).disabled=!on;
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
// into the dialog as they are; what the file leaves out stays as the dialog
// has it. Nothing is saved until the dialog is.
const SWITCHES=['auto_answer','aec','high_pass','agc','detail_log','pbx_only','program_integration','browser_integration','browser_dial_confirm'];
const CARRIED=['microphone_gain','speaker_gain','auto_record'];
const CHOICES=['transport','media_encryption','noise_suppression','incoming_action','language'];
const BUTTON_FIELDS=['title','kind','number','transfer','pickup'];
const isChoice=key=>CHOICES.includes(key)||/^button_\d+_kind$/.test(key);
// One value into the dialog, and whether the dialog shows it.
function showSetting(key,value){
  if(SWITCHES.includes(key))$(key).checked=value;
  else if(CARRIED.includes(key))carried[key]=value;
  else if(key==='codecs')setCodecs(value);
  else if(isChoice(key))setChoice($(key),value);
  else if($(key))$(key).value=value;
}
function shows(key,value){
  if(SWITCHES.includes(key))return $(key).checked===value;
  if(CARRIED.includes(key))return carried[key]===value;
  // In the dialog's names; an empty list offers every codec, and the dialog ticks them all.
  if(key==='codecs'){const {inUse,dropped}=codecNames(value);return !dropped.length&&tickedCodecs()===inUse.join(',');}
  return !!$(key)&&$(key).value===String(value);
}
// What the dialog holds, to put back when a file is not taken after all.
function snapshot(){
  const fields=[...$('settings-form').querySelectorAll('input,select')].map(el=>({el,value:el.value,checked:el.checked,options:el.tagName==='SELECT'?[...el.options]:null}));
  return {fields,codecs:[...$('codec-list').children],carried:{...carried}};
}
function restore(before){
  for(const {el,options} of before.fields)if(options)el.replaceChildren(...options);
  $('codec-list').replaceChildren(...before.codecs);
  for(const {el,value,checked} of before.fields){el.value=value;el.checked=checked;}
  carried=before.carried;
  syncEncryptionChoices();syncIntegration();for(const n of BUTTON_INDEXES)syncButtonRow(n);
}
// Every value the file gives is shown as it is, or none is taken: a file
// that would change the dialog in a way it does not say (a choice the list
// lacks read as nothing, SDES left on UDP) leaves the dialog as it was.
function applyImport(file){
  const settings=file.settings||{},account=file.account||{},buttons=(file.buttons||[]).filter(b=>BUTTON_INDEXES.includes(b.n));
  // The encryption as it would stand with the file in, from the file and
  // what the dialog holds: keys in the signalling need TLS.
  if('transport' in settings||'media_encryption' in settings){
    const transport=settings.transport??$('transport').value,media=settings.media_encryption??$('media_encryption').value;
    if(transport!=='tls'&&(media==='sdes'||media==='osrtp'))throw t('SETTINGS_IMPORT_NEEDS_TLS');
  }
  const given=[
    ...Object.entries(settings),
    ...buttons.flatMap(b=>BUTTON_FIELDS.filter(field=>field in b).map(field=>['button_'+b.n+'_'+field,b[field]])),
    ...['server','port','extension'].filter(key=>key in account).map(key=>[key,account[key]]),
  ];
  const before=snapshot();
  for(const [key,value] of given)showSetting(key,value);
  syncEncryptionChoices();syncIntegration();for(const b of buttons)syncButtonRow(b.n);
  const notShown=given.filter(([key,value])=>!shows(key,value)).map(([key])=>key);
  if(notShown.length){restore(before);throw fill('SETTINGS_IMPORT_NOT_SHOWN',notShown.join(', '));}
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
    pbx_only:$('pbx_only').checked,program_integration:$('program_integration').checked,
    browser_integration:$('browser_integration').checked,browser_dial_confirm:$('browser_dial_confirm').checked,
    transport:$('transport').value,media_encryption:$('media_encryption').value,
    // Every codec in the usual order is what an empty setting means: the
    // empty one is kept, so that a save that changed nothing changes nothing.
    codecs:tickedCodecs()===CODEC_NAMES.join(',')&&!state.settings.codecs?'':tickedCodecs(),
    ca_file:$('ca_file').value.trim(),
    sound_ring:$('sound_ring').value.trim(),sound_ringback:$('sound_ringback').value.trim(),sound_busy:$('sound_busy').value.trim(),sound_notfound:$('sound_notfound').value.trim(),sound_error:$('sound_error').value.trim(),
  };
}
export function init(){
  $('settings-button').addEventListener('click',openSettings);
  $('close-settings').addEventListener('click',()=>{$('password').value='';carried={};$('configuration').close();});
  // The close event comes a moment after the dialog closed: one that opened
  // again meanwhile (trimmed, from the button editing) is left as it is.
  $('configuration').addEventListener('close',()=>{if(!$('configuration').open)untrim();});
  // A tab by click, and the next or previous by the arrow keys, as a tab list does.
  const tabs=[...document.querySelectorAll('.settings-tabs [role="tab"]')];
  for(const tab of tabs){
    tab.addEventListener('click',()=>selectTab(tab.dataset.tab));
    tab.addEventListener('keydown',e=>{
      const step=e.key==='ArrowRight'?1:e.key==='ArrowLeft'?-1:0;
      if(!step)return;
      e.preventDefault();
      const next=tabs[(tabs.indexOf(tab)+step+tabs.length)%tabs.length];
      selectTab(next.dataset.tab);next.focus();
    });
  }
  // The save says whether it reconnects, as the dialog is changed.
  for(const type of ['input','change'])$('settings-form').addEventListener(type,()=>{if($('configuration').open)updateSaveLabel();});
  invoke('restart_settings').then(names=>{restartNames=names;}).catch(e=>logUi('restart settings',e));
  // A codec moves one row up or down; the rows' order is the order offered.
  $('codec-list').addEventListener('click',e=>{const button=e.target.closest('button[data-move]');if(!button)return;const row=button.closest('li'),list=row.parentElement;if(button.dataset.move==='-1'){if(row.previousElementSibling)list.insertBefore(row,row.previousElementSibling);}else if(row.nextElementSibling)list.insertBefore(row.nextElementSibling,row);});
  $('configuration').addEventListener('cancel',e=>{if(busy)e.preventDefault();else $('password').value='';});
  $('transport').addEventListener('change',()=>{
    // 既定のポートを使っているときだけ、方式に合わせて入れ替える。
    const tls=$('transport').value==='tls',port=$('port');
    if(port.value.trim()===(tls?'5060':'5061'))port.value=tls?'5061':'5060';
    syncEncryptionChoices();
  });
  $('media_encryption').addEventListener('change',syncEncryptionChoices);
  $('program_integration').addEventListener('change',syncIntegration);
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
    $('settings-error').hidden=true;$('settings-file-note').hidden=true;
    try{const file=await invoke('import_settings_file');if(file)applyImport(file);}
    catch(e){showFileError('import settings',e);}
  });
  $('export-settings').addEventListener('click',async()=>{
    $('settings-error').hidden=true;$('settings-file-note').hidden=true;
    try{const path=await invoke('export_settings_file');if(path){const note=$('settings-file-note');note.textContent=fill('SETTINGS_EXPORTED',path);note.hidden=false;}}
    catch(e){showFileError('export settings',e);}
  });
  $('calibrate-aec').addEventListener('click',()=>calibrateAec(false));
  $('calibrate-aec-careful').addEventListener('click',()=>calibrateAec(true));
  $('settings-form').addEventListener('submit',async e=>{
    e.preventDefault();$('settings-error').hidden=true;
    const settings=collectSettings();
    // Trimmed to a button: the buttons alone are saved, taken in at once.
    if(trimmedTo){
      try{await run(()=>invoke('save_buttons',{buttons:settings.buttons}));$('configuration').close();}
      catch(e){$('settings-error').textContent=t(String(e));$('settings-error').hidden=false;}
      return;
    }
    // No codec ticked is not saved: an empty list would offer all of them.
    if(!tickedCodecs()){$('settings-error').textContent=t('SETTINGS_CODECS_INVALID');$('settings-error').hidden=false;return;}
    const account=collectAccount();
    // The save answers with a notice when it has something to say (the
    // reconnect waits for the calls to end).
    let notice='';
    try{await run(async()=>{notice=await invoke('save_configuration',{settings,account});});$('password').value='';$('configuration').close();if(notice)setError(notice);}
    catch(e){$('settings-error').textContent=t(String(e));$('settings-error').hidden=false;}
    finally{account.password='';}
  });
}
