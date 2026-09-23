'use strict';
const $=id=>document.getElementById(id);
const invoke=(command,args={})=>window.__TAURI__.core.invoke(command,args);
const kinds=['microphone','speaker'];
// The web view keeps no log of its own, so failures go to the app log. The same
// text repeats at most once a minute per place: a failing poll runs three times
// a second and must not fill the log.
const loggedUi={};
function logUi(where,detail){
  const text=where+': '+nameOf(detail&&detail.message?detail.message:detail??'').code,now=Date.now(),last=loggedUi[where];
  if(last&&last.text===text&&now-last.at<60000)return;
  loggedUi[where]={text,at:now};
  invoke('log_ui',{text}).catch(()=>{});
}
window.addEventListener('error',e=>logUi('script error',(e.message||'')+' '+(e.filename||'')+':'+(e.lineno||0)));
window.addEventListener('unhandledrejection',e=>logUi('unhandled rejection',e.reason));
// 文言は locales/ にある。ここは、どの言語の表を使うかだけを決める。
// 完全一致、なければ言語の主要部分、それでもなければ日本語。表に無いキーは
// 日本語へ、日本語にも無ければキーそのものを出す。画面が空になるよりよい。
const TEXTS=window.KSIP_TEXTS||{};
let texts=TEXTS.ja||{messages:{}};
function languageFor(tag){
  const wanted=String(tag||'').trim();
  if(!wanted)return 'ja';
  if(TEXTS[wanted])return wanted;
  // 中国語は国ではなく文字で分かれるので、地域から書体へ寄せる。
  const chinese={'zh':'zh-TW','zh-HK':'zh-TW','zh-MO':'zh-TW','zh-Hant':'zh-TW','zh-SG':'zh-CN','zh-Hans':'zh-CN'};
  if(chinese[wanted]&&TEXTS[chinese[wanted]])return chinese[wanted];
  const primary=wanted.split('-')[0];
  for(const tag of Object.keys(TEXTS))if(tag===primary||tag.split('-')[0]===primary)return tag;
  return TEXTS.ja?'ja':Object.keys(TEXTS)[0]||'ja';
}
// 選んだ言語を日本語の上に重ねる。訳が無いキーは日本語で出る。画面のどこかが
// 空になるより、そこだけ日本語で出るほうがよい。
let language='ja';
function useLanguage(tag){
  language=languageFor(tag);
  const base=TEXTS.ja||{},chosen=TEXTS[language]||base;
  texts={};
  for(const key of new Set([...Object.keys(base),...Object.keys(chosen)])){
    const japanese=base[key],translated=chosen[key];
    texts[key]=japanese&&typeof japanese==='object'?{...japanese,...(translated||{})}:translated??japanese;
  }
  applyText();
}
// 画面の文字は data-i18n で名前だけを持ち、言語を決めた時点で流し込む。
const TEXT_ATTRIBUTES=[['data-i18n-placeholder','placeholder'],['data-i18n-title','title'],['data-i18n-label','aria-label']];
function applyText(){
  for(const node of document.querySelectorAll('[data-i18n]'))node.textContent=t(node.getAttribute('data-i18n'));
  for(const [source,target] of TEXT_ATTRIBUTES)
    for(const node of document.querySelectorAll('['+source+']'))node.setAttribute(target,t(node.getAttribute(source)));
  document.documentElement.lang=language;
}
// The button rows of the settings are one template stamped out: six for the
// phone, the rest for the panel beside it. Stamped before the wording is
// applied, so that it reaches them too.
const BUTTON_MAIN=6,BUTTON_COUNT=30,BUTTON_INDEXES=Array.from({length:BUTTON_COUNT},(_,i)=>i+1);
(function buildButtonSets(){
  const html=$('button-set-template').innerHTML;
  for(const n of BUTTON_INDEXES)$(n<=BUTTON_MAIN?'button-sets':'extended-sets').insertAdjacentHTML('beforeend',html.replaceAll('button_N_','button_'+n+'_').replace('>N<','>'+n+'<'));
})();
useLanguage(navigator.language);
// エンジンと本体は「起きたことの名前」だけを返す。値を伴うものはJSONで届く。
// 名前を知らないときは、その名前をそのまま出す（古い通話履歴もこれで読める）。
function nameOf(value){
  const text=String(value??'');
  if(!text.startsWith('{'))return {code:text,args:[]};
  try{const parsed=JSON.parse(text);return {code:parsed.code||'',args:parsed.args||[]};}
  catch{return {code:text,args:[]};}
}
const fill=(code,...args)=>t(JSON.stringify({code,args:args.map(String)}));
function t(value){
  const {code,args}=nameOf(value);
  if(!code)return '';
  const sentence=texts.messages[code];
  if(!sentence)return code;
  return sentence.replace(/\{(\d+)\}/g,(_,index)=>t(args[index]??''));
}
let state={running:false,calls:[],devices:[],transfer:{},account:{},settings:{}}, ready=false,busy=false,selected=1,error='',first=true;
let deviceKey='',activePanel='history';
let logRows=[],logCursor=0,historyRows=[],historySequence=-1;
const peakPending={microphone:false,speaker:false};
const volumes=Object.fromEntries(kinds.map(k=>[k,{pending:false,dragging:false,available:false,muted:false,sequence:0,queued:null}]));
const callAt=n=>state.calls.find(c=>c.line===n);
const current=()=>callAt(selected);
const peerLabel=c=>c?.peer?.replace(/^sip:/,'').split('@')[0] || '—';
const status=c=>c?(c.held?texts.callHeld:texts.callState[c.state]||c.state):texts.callIdle;
const two=value=>String(value).padStart(2,'0');
function logTime(iso){const at=new Date(iso);return isNaN(at)?iso:(at.getMonth()+1)+'/'+at.getDate()+' '+at.getHours()+':'+two(at.getMinutes())+':'+two(at.getSeconds());}
const logMessage=row=>row.code?t(row.code&&row.args?.length?JSON.stringify({code:row.code,args:row.args}):row.code):row.text;
function logText(row){const body=logMessage(row);return body?logTime(row.time)+' ['+row.src+'] '+body:'';}
// One box narrows both panels: the history by number, the log by its source
// tag and its words. Empty means everything, as before.
const filterText=()=>$('panel-filter').value.trim().toLowerCase();
const passesFilter=(...parts)=>{const wanted=filterText();return !wanted||parts.some(part=>String(part||'').toLowerCase().includes(wanted));};
const historyNumber=peer=>(peer||'—').replace(/^sip:/,'').split('@')[0];
// Newest first, as the call history is shown.
const logBody=()=>logRows.filter(row=>passesFilter(row.src,logMessage(row))).map(logText).reverse().join('\n');
// The log tab is left alone while the person is in it, selecting lines to
// copy: rewriting the text would drop the selection. Lines keep arriving
// meanwhile and are shown the moment the tab is left.
let logShown='';
function logPaused(){
  const logs=$('logs'),selection=document.getSelection();
  return document.activeElement===logs||(!!selection&&!selection.isCollapsed&&logs.contains(selection.anchorNode));
}
function renderLogs(){
  const paused=activePanel==='logs'&&logPaused();
  $('logs-paused').hidden=!paused;
  $('filter-active').hidden=!filterText();
  if(activePanel!=='logs'||paused)return;
  const body=logBody();
  if(body!==logShown){logShown=body;$('logs').textContent=body;}
}
$('logs').addEventListener('blur',renderLogs);
document.addEventListener('selectionchange',renderLogs);
// Ctrl+A in the log selects the log, not the whole window.
$('logs').addEventListener('keydown',e=>{
  if(e.key.toLowerCase()!=='a'||!(e.ctrlKey||e.metaKey))return;
  e.preventDefault();const range=document.createRange();range.selectNodeContents($('logs'));
  const selection=document.getSelection();selection.removeAllRanges();selection.addRange(range);
});
const durationText=seconds=>String(Math.floor(seconds/60)).padStart(2,'0')+':'+String(seconds%60).padStart(2,'0');
const metric=(value,unit,digits=1)=>Number.isFinite(value)?value.toFixed(digits)+unit:'—';
const count=value=>Number.isFinite(value)?value.toLocaleString(language):'—';
const codecNames={opus:'Opus',G722:'G.722',PCMU:'G.711 μ-law',PCMA:'G.711 A-law'};
const encryptionNames={srtp:'SRTP（SDES）',dtls_srtp:'SRTP（DTLS）'};
function codecName(reported){
  const [name,rate]=reported.split(' ');
  const hz=parseInt(rate,10);
  return (codecNames[name]||name)+(Number.isFinite(hz)?' '+hz/1000+' kHz':'');
}
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
function callSummary(call){
  if(!call||!call.codec)return '';
  const parts=[codecName(call.codec)];
  // DTLS finishes its handshake a moment after the call is answered, so the
  // verdict waits rather than flashing the wrong answer first.
  if(call.secure)parts.push(encryptionNames[state.media_encryption]||'SRTP');
  else if(state.media_encryption!=='dtls_srtp'||call.duration>=2)parts.push(t('MEDIA_UNENCRYPTED'));
  return ' · '+parts.join(' · ');
}
// The buttons a site defined, with their position, leaving out the unused ones.
const configuredButtons=()=>(state.settings.buttons||[]).map((b,i)=>({...b,index:i+1})).filter(b=>b.kind);
// A switch on the phone itself has a name of its own in the window's language,
// used while the title is left empty; the other kinds fall back to the number.
const defaultTitle=kind=>kind==='dnd'?t('BUTTON_TITLE_DND'):kind==='mwi'?t('BUTTON_TITLE_MWI'):'';
const buttonTitle=b=>b.title||defaultTitle(b.kind)||b.number;
// A URI may be written between angle brackets; the engine reports it bare.
const address=text=>String(text||'').trim().replace(/^<\s*/,'').replace(/\s*>$/,'');
const watchState=number=>state.parking?.find(slot=>slot.number===address(number))?.state||'UNKNOWN';
function ask(message){
  // The page's own confirm() is prefixed with the origin, which means nothing
  // to the person reading it, so the question is asked inside the window.
  return new Promise(resolve=>{
    const dialog=$('confirm');
    $('confirm-text').textContent=message;
    const close=answer=>{
      dialog.removeEventListener('cancel',cancelled);
      $('confirm-ok').removeEventListener('click',accepted);
      $('confirm-cancel').removeEventListener('click',refused);
      if(dialog.open)dialog.close();
      resolve(answer);
    };
    const accepted=()=>close(true),refused=()=>close(false);
    const cancelled=event=>{event.preventDefault();close(false);};
    $('confirm-ok').addEventListener('click',accepted);
    $('confirm-cancel').addEventListener('click',refused);
    dialog.addEventListener('cancel',cancelled);
    dialog.showModal();
    $('confirm-ok').focus();
  });
}
// A call that was recorded can be played back: the file is opened with
// whatever Windows plays sound with. The button is only drawn while the
// file is there, which the app checks as it hands the history over.
function playButton(name){
  return recordingButton('history-play',t('HISTORY_PLAY'),'<path d="M4.5 2.6v10.8L13.5 8z"/>','open_recording',name);
}
// The same file shown in Explorer, selected, as a download bar offers.
function locationButton(name){
  return recordingButton('history-location',t('HISTORY_LOCATION'),'<path d="M1.8 4.6A1.3 1.3 0 0 1 3.1 3.3h3l1.5 1.5h5.3a1.3 1.3 0 0 1 1.3 1.3v6.1a1.3 1.3 0 0 1-1.3 1.3H3.1a1.3 1.3 0 0 1-1.3-1.3z"/>','open_recording_location',name);
}
// An icon button in a history row: the wording lives in the title and the
// name read out, as the copy button's does.
function recordingButton(className,label,icon,command,name){
  const button=document.createElement('button');
  button.type='button';button.className=className;
  button.title=label;button.setAttribute('aria-label',label);
  button.innerHTML='<svg viewBox="0 0 16 16" aria-hidden="true">'+icon+'</svg>';
  button.addEventListener('click',async event=>{
    event.stopPropagation();
    try{await invoke(command,{name});}
    catch(e){error=String(e);logUi(command,e);render();}
  });
  button.addEventListener('dblclick',event=>event.stopPropagation());
  return button;
}
function copyButton(number){
  const button=document.createElement('button');
  button.type='button';button.className='history-copy';
  button.title=t('COPY_NUMBER');button.setAttribute('aria-label',fill('COPY_NUMBER_LABEL',number));
  button.innerHTML='<svg viewBox="0 0 16 16" aria-hidden="true" fill="none" stroke="currentColor" stroke-width="1.4">'
    +'<rect x="5.2" y="5.2" width="8" height="9" rx="1.4"/><path d="M10.8 3.2H3.6a1.4 1.4 0 0 0-1.4 1.4v7.2"/></svg>';
  button.addEventListener('click',async event=>{
    // The row dials on a double click; copying must not start a call.
    event.stopPropagation();
    try{
      await invoke('copy_text',{text:number});
      button.title=t('COPY_DONE');
      setTimeout(()=>{button.title=t('COPY_NUMBER');},1500);
    }catch(e){error=String(e);logUi('copy number',e);render();}
  });
  button.addEventListener('dblclick',event=>event.stopPropagation());
  return button;
}
function renderHistory(){
  const history=historyRows.filter(item=>passesFilter(historyNumber(item.peer)));
  const panel=$('call-history');panel.replaceChildren();panel.className='history'+(history.length?'':' empty');
  if(!history.length){const empty=document.createElement('div');empty.className='history-empty';empty.textContent=t(historyRows.length?'HISTORY_NO_MATCH':'HISTORY_EMPTY');panel.append(empty);return;}
  for(const item of history){const row=document.createElement('div');row.className='history-row';row.title=t('HISTORY_DIAL_HINT');row.addEventListener('dblclick',()=>dialHistory(item.peer));const direction=document.createElement('strong');direction.textContent=t(item.direction);const time=document.createElement('time');time.textContent=new Date(item.ended_at*1000).toLocaleString(language,{month:'numeric',day:'numeric',hour:'2-digit',minute:'2-digit'});const cell=document.createElement('div');cell.className='history-peer-cell';const peer=document.createElement('span');peer.className='history-peer';const number=historyNumber(item.peer);peer.textContent=number;cell.append(peer);if(item.peer)cell.append(copyButton(number));if(item.recording)cell.append(playButton(item.recording),locationButton(item.recording));const meta=document.createElement('span');meta.className='history-meta';meta.textContent=durationText(item.duration||0);row.append(direction,time,cell,meta);panel.append(row);}
}
function dialHistory(peer){
  dial((peer||'').replace(/^sip:/i,'').split(/[;@]/)[0]);
}
// Every way of placing a call ends here: the dial box, a button, the history
// and a link. The selected line is used while it is free, otherwise the free
// one; the number goes into the dial box, so that what was dialled can be seen
// afterwards. The box and the buttons are disabled while something else is
// going on, so of the refusals only a link ever meets one.
function dial(number){
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
function answer(){
  const ringing=current()?.state==='INCOMING'?current():state.calls.find(c=>c.state==='INCOMING');
  if(!ringing){logUi('call','ANSWER without an incoming call');return;}
  selected=ringing.line;render();return act('answer',ringing.id,'',ringing.line);
}
function hangup(){
  if(!current()){logUi('call','HANGUP without a call');return;}
  return act('hangup');
}
function render(){
  // The first row says who this phone is and how it is; the second where it
  // is connected, which only means something while it is registered.
  const registered=state.registration==='REGISTER_OK';
  const paused=state.registration==='UNREGISTERED'||state.registration==='UNREGISTERING';
  $('account-label').textContent=state.account.extension;
  $('server-label').textContent=state.account.server?state.account.server+':'+state.account.port:'';
  $('connection-server').hidden=paused||!state.account.extension;
  $('registration').textContent=((state.registration==='UNCONFIGURED'?(state.account.has_password?texts.registrationPreparing:texts.registrationUnconfigured):texts.registration[state.registration])||texts.registrationStarting)+(state.dnd&&registered?' · '+t('DND_ACTIVE'):'');
  $('registration').className='reg-'+String(state.registration||'').toLowerCase();$('status-light').className=registered?'online':paused?'paused':state.registration==='REGISTER_FAIL'?'fault':'';
  $('transport-label').textContent=registered?state.transport||'':'';
  $('settings-button').disabled=!ready||busy||state.calls.length>0;
  $('reconnect').disabled=!ready||busy||state.calls.length>0||!state.account.has_password;
  // Unregistering is for a meeting: it stops calls arriving without closing KSIP.
  $('unregister').disabled=!ready||busy||state.calls.length>0||!state.running||state.registration==='UNREGISTERED';
  for(let n=1;n<=2;n++){const c=callAt(n),seconds=c?.duration||0;$('line-'+n).className='line'+(selected===n?' selected':'')+(c?.state==='INCOMING'?' incoming':'')+' call-'+(c?(c.held?'held':String(c.state).toLowerCase()):'idle');$('line-'+n).setAttribute('aria-pressed',String(selected===n));$('line-'+n).disabled=busy||!!state.transfer.pending;$('line-'+n+'-status').textContent=status(c);$('line-'+n+'-peer').textContent=peerLabel(c);$('line-'+n+'-duration').textContent=String(Math.floor(seconds/60)).padStart(2,'0')+':'+String(seconds%60).padStart(2,'0');}
  const c=current();
  $('target').disabled=busy||!!c;
  $('dial').disabled=busy||!!c||state.registration!=='REGISTER_OK'||!!state.transfer.pending;
  $('answer').disabled=busy||c?.state!=='INCOMING'||!!state.transfer.pending;
  $('hangup').disabled=busy||!c;
  $('hold').disabled=busy||c?.state!=='ESTABLISHED'||!!state.transfer.pending;
  $('hold').textContent=t(c?.held?'HOLD_RESUME':'HOLD');
  renderButtons(c);
  $('transfer').disabled=busy||!!state.transfer.pending||![callAt(1),callAt(2)].every(c=>c?.state==='ESTABLISHED');
  const outcome=state.transfer.outcome||'';
  $('transfer-status').className='hint'+(outcome?' '+outcome.toLowerCase().replace(/_/g,'-'):'');
  $('transfer-status').textContent=t(outcome);
  $('record').disabled=!ready||busy;$('record').textContent=t(state.settings.auto_record?'RECORD_ON':'RECORD_OFF');$('record').classList.toggle('active',!!state.settings.auto_record);$('record').setAttribute('aria-pressed',String(!!state.settings.auto_record));
  $('record-status').classList.toggle('recording',!!state.recording);$('record-status').textContent=t(state.recording?'RECORD_RECORDING':state.converting?'RECORDING_CONVERTING':state.settings.auto_record?'RECORD_AUTO_ON':'RECORD_AUTO_OFF');
  $('aec-label').textContent=state.settings.aec?'ON':'OFF';
  // A codec and an encryption belong to a call, so outside one this stays empty.
  $('codec-label').textContent=callSummary(current());
  const metrics=state.audio_processing_stats;
  const showMetrics=!!state.settings.aec&&!!state.aec_active&&state.calls.some(call=>call.state==='ESTABLISHED')&&!!metrics;
  $('aec-metrics').hidden=!showMetrics;
  $('aec-state').hidden=!showMetrics;
  if(showMetrics){
    const errors=(metrics.render_errors||0)+(metrics.capture_errors||0),hasRender=(metrics.render_frames||0)>0&&Number.isFinite(metrics.render_rms_dbfs)&&metrics.render_rms_dbfs>-65,hasCapture=(metrics.capture_frames||0)>0,hasCaptureSignal=Number.isFinite(metrics.capture_input_rms_dbfs)&&metrics.capture_input_rms_dbfs>-65,converged=(metrics.capture_frames||0)>500&&Number.isFinite(metrics.delay_ms)&&metrics.delay_ms>0&&Number.isFinite(metrics.echo_return_loss_enhancement)&&metrics.echo_return_loss_enhancement>3;
    $('aec-state').textContent=t(errors?'AEC_ERROR':!hasCapture?'AEC_WAITING_MICROPHONE':!hasRender?'AEC_WAITING_REFERENCE':!hasCaptureSignal?'AEC_LOW_INPUT':converged?'AEC_CONVERGED':'AEC_ESTIMATING');
    $('aec-state').classList.toggle('error',errors>0);$('aec-state').classList.toggle('ready',!errors&&converged);
    const residual=Number.isFinite(metrics.residual_echo_likelihood)?fill('AEC_RESIDUAL',metric(metrics.residual_echo_likelihood*100,'%',0),
      Number.isFinite(metrics.residual_echo_likelihood_recent_max)?fill('AEC_RESIDUAL_MAX',metric(metrics.residual_echo_likelihood_recent_max*100,'%',0)):''):'';
    const divergent=Number.isFinite(metrics.divergent_filter_fraction)?fill('AEC_DIVERGENT',metric(metrics.divergent_filter_fraction*100,'%',0)):'';
    const median=Number.isFinite(metrics.delay_median_ms)?fill('AEC_MEDIAN',metrics.delay_median_ms,
      Number.isFinite(metrics.delay_standard_deviation_ms)?fill('AEC_DEVIATION',metrics.delay_standard_deviation_ms):''):'';
    $('aec-core').textContent=fill('AEC_CORE',metric(metrics.echo_return_loss_enhancement,' dB'),metric(metrics.echo_return_loss,' dB'),residual,divergent);
    $('aec-delay').textContent=fill('AEC_DELAY',metric(metrics.delay_ms,' ms',0),metric(metrics.stream_delay_ms,' ms',0),
      t(metrics.stream_delay_from_device?'AEC_DELAY_MEASURED':'AEC_DELAY_CONFIGURED'),median);
    $('aec-levels').textContent=fill('AEC_LEVELS',metric(metrics.render_rms_dbfs,' dBFS'),metric(metrics.capture_input_rms_dbfs,' dBFS'),metric(metrics.capture_output_rms_dbfs,' dBFS'));
    $('aec-flow').textContent=fill('AEC_FLOW',count(metrics.render_frames),count(metrics.capture_frames),count(errors),
      errors?fill('AEC_FLOW_DETAIL',count(metrics.render_errors),count(metrics.capture_errors)):'');
  }else{
    for(const id of ['aec-core','aec-delay','aec-levels','aec-flow'])$(id).textContent='';
  }
  $('error').textContent=t(error)||t(state.error)||'';$('error').hidden=!$('error').textContent;
  $('call-history').hidden=activePanel!=='history';$('logs').hidden=activePanel!=='logs';$('history-tab').classList.toggle('active',activePanel==='history');$('logs-tab').classList.toggle('active',activePanel==='logs');$('history-tab').setAttribute('aria-selected',String(activePanel==='history'));$('logs-tab').setAttribute('aria-selected',String(activePanel==='logs'));renderLogs();
  for(const kind of kinds){$(kind+'-volume').disabled=busy||!volumes[kind].available;$(kind+'-mute').disabled=busy||!volumes[kind].available;$(kind).disabled=busy||state.calls.length>0||!state.account.has_password;}
  $('refresh-devices').disabled=busy||state.calls.length>0;
  $('save-settings').disabled=busy;$('close-settings').disabled=busy;for(const id of ['calibrate-aec','calibrate-aec-careful'])$(id).disabled=busy||state.calls.length>0;
}
function deviceOptions(kind,selected){
  const options=[new Option(t(kind==='microphone'?'AUDIO_MICROPHONE_DEFAULT':'AUDIO_SPEAKER_DEFAULT'),'default')];
  for(const d of state.devices.filter(d=>d.kind===kind)) options.push(new Option(d.name,d.id));
  if(!options.some(o=>o.value===selected))options.push(new Option(t('AUDIO_DEVICE_SAVED_MISSING'),selected));
  $(kind).replaceChildren(...options);$(kind).value=selected;
}
function update(snapshot){
  // A saved language wins over the one Windows reports, and changing it in the
  // settings takes effect at once.
  const wanted=snapshot.settings?.language||navigator.language;
  if(languageFor(wanted)!==language)useLanguage(wanted);
  state=snapshot;ready=true;
  const key=JSON.stringify(state.devices);
  if(deviceKey!==key){for(const kind of kinds)deviceOptions(kind,state.settings[kind]||'default');deviceKey=key;}
  for(const kind of kinds)if($(kind).value!==state.settings[kind])$(kind).value=state.settings[kind];
  render();
  if(first){first=false;if(!state.account.has_password)openSettings();}
}
function openSettings(){
  for(const key of ['server','port','extension','auth_user'])$(key).value=state.account[key]??'';
  for(const key of ['sip_port','rtp_port'])$(key).value=state.settings[key];
  for(const n of BUTTON_INDEXES){const b=(state.settings.buttons||[])[n-1]||{};$('button_'+n+'_title').value=b.title||'';$('button_'+n+'_kind').value=b.kind||'';$('button_'+n+'_number').value=b.number||'';$('button_'+n+'_transfer').value=b.transfer||'';$('button_'+n+'_pickup').value=b.pickup||'';syncButtonRow(n);}
  fillAdapters(state.settings.network_adapter||'');
  for(const key of ['sound_ring', 'sound_ringback', 'sound_callwaiting', 'sound_busy', 'sound_notfound', 'sound_error'])$(key).value=state.settings[key]||'';
  $('password').value='';$('register_interval').value=state.settings.register_interval??300;$('detail_log').checked=!!state.settings.detail_log;$('shortcut_window').value=state.settings.shortcut_window||'';$('shortcut_call').value=state.settings.shortcut_call||'';$('incoming_action').value=state.settings.incoming_action==='notify'?'notify':'show';$('tray_after_call').value=state.settings.tray_after_call??-1;$('language').value=state.settings.language||'';$('browser_integration').checked=!!state.settings.browser_integration;$('browser_dial_confirm').checked=state.settings.browser_dial_confirm!==false;$('transport').value=state.settings.transport||'udp';$('media_encryption').value=state.settings.media_encryption||'';$('ca_file').value=state.settings.ca_file||'';$('auto_answer').checked=!!state.settings.auto_answer;$('aec').checked=state.settings.aec;$('aec_delay_ms').value=state.settings.aec_delay_ms??20;$('calibration-status').textContent=t('SETTINGS_CALIBRATION_IDLE');
  $('password-hint').textContent=t(state.account.has_password?'SETTINGS_PASSWORD_SAVED':'SETTINGS_PASSWORD_HINT');
  $('settings-error').hidden=true;$('configuration').showModal();
}
async function run(fn){
  if(busy)return;busy=true;error='';render();
  try{await fn();update(await invoke('snapshot'));}
  catch(e){error=String(e);logUi('command',e);throw e;}
  finally{busy=false;render();}
}
async function act(name,id=current()?.id||'',value='',line=selected){
  try{await run(()=>invoke('action',{name,id,value,line}));}catch{}
}
for(let n=1;n<=2;n++)$('line-'+n).addEventListener('click',async()=>{
  try{await run(async()=>{if(state.running)await invoke('action',{name:'select',id:callAt(n)?.id||'',value:'',line:n});selected=n;});}catch{}
});
$('dial-form').addEventListener('submit',e=>{e.preventDefault();dial($('target').value.trim());});
$('answer').addEventListener('click',()=>answer());
$('hangup').addEventListener('click',()=>hangup());
$('hold').addEventListener('click',()=>act(current()?.held?'resume':'hold'));
// The buttons are drawn from the settings: only the configured ones, in
// order, each keeping its own id so that a test can find it by position.
// The phone's six buttons and the panel's are drawn the same way; the panel
// and the wider window only exist while a panel button is set.
function renderButtons(c){
  const all=configuredButtons(),extended=all.filter(b=>b.index>BUTTON_MAIN);
  document.body.classList.toggle('extended',extended.length>0);
  renderButtonBox($('custom-actions'),all.filter(b=>b.index<=BUTTON_MAIN),c);
  renderButtonBox($('extended-actions'),extended,c);
}
function renderButtonBox(box,configured,c){
  box.hidden=!configured.length;
  const key=JSON.stringify(configured);
  if(box.dataset.key!==key){
    box.replaceChildren(...configured.map(b=>{const button=document.createElement('button');button.type='button';button.id='custom-'+b.index;button.className='custom-'+b.kind;const title=document.createElement('strong');const status=document.createElement('small');status.id='custom-'+b.index+'-status';button.append(title,status);button.addEventListener('click',()=>useButton(b.index));return button;}));
    box.dataset.key=key;
  }
  const active=c?.state==='ESTABLISHED'&&!c.held,freeLine=[1,2].some(line=>!callAt(line)),registered=state.registration==='REGISTER_OK';
  for(const b of configured){
    const button=$('custom-'+b.index),watched=b.kind==='dial'||b.kind==='park',s=watched?watchState(b.number):'';
    let status,enabled,needsPhone=true;
    if(b.kind==='transfer'){status=fill('BUTTON_TRANSFER_TO',b.number);enabled=active;}
    else if(b.kind==='dial'){status=b.number+' · '+(texts.park[s]||texts.park.UNKNOWN);enabled=freeLine;}
    else if(b.kind==='open'){status=b.number;enabled=true;needsPhone=false;}
    else if(b.kind==='dnd'){status=t(state.dnd?'DND_ON_STATUS':'DND_OFF_STATUS');enabled=true;}
    else if(b.kind==='mwi'){const m=state.mwi||{};status=m.new>0?fill('MWI_NEW',m.new):t('MWI_NONE');enabled=freeLine;}
    else{status=b.number+' · '+(texts.park[s]||texts.park.UNKNOWN);enabled=s!=='UNKNOWN'&&(active?s==='IDLE':s==='INUSE'&&freeLine);}
    button.firstChild.textContent=buttonTitle(b);
    // A button's children are presentational to assistive technology, so its
    // name carries the status as well as the title.
    button.setAttribute('aria-label',buttonTitle(b)+' '+status);
    $('custom-'+b.index+'-status').textContent=status;
    button.classList.toggle('occupied',watched&&s==='INUSE');
    button.classList.toggle('dnd',b.kind==='dnd'&&!!state.dnd);
    button.classList.toggle('mwi-new',b.kind==='mwi'&&(state.mwi?.new||0)>0);
    button.disabled=busy||!enabled||(needsPhone&&(!registered||!!state.transfer.pending));
  }
}
async function useButton(n){
  const b=configuredButtons().find(button=>button.index===n);
  if(!b)return;
  if(b.kind==='open'){
    try{await invoke('open_link',{url:b.number});}
    catch(e){error=String(e);logUi('open link',e);render();}
    return;
  }
  if(b.kind==='dnd'){await act('dnd','',state.dnd?'off':'on');return;}
  if(b.kind==='mwi'){await dial(b.number);return;}
  const c=current(),active=c?.state==='ESTABLISHED'&&!c.held,s=b.kind==='transfer'?'':watchState(b.number);
  if(b.kind==='transfer'){if(active)await act('blind_transfer',c.id,b.number);return;}
  if(b.kind==='park'&&active&&s==='IDLE'){await act('blind_transfer',c.id,b.transfer||b.number);return;}
  // A watched number in use is picked up rather than called: the pickup
  // target, which is the number itself unless the button says otherwise.
  if(b.kind==='dial'||(b.kind==='park'&&s==='INUSE'))await dial(s==='INUSE'?b.pickup||b.number:b.number);
}
$('transfer').addEventListener('click',()=>act('transfer',callAt(1)?.id||'',callAt(2)?.id||'',1));
$('record').addEventListener('click',()=>act('auto_record','',state.settings.auto_record?'off':'on'));
$('open-recordings').addEventListener('click',()=>invoke('open_recordings').catch(e=>{error=String(e);logUi('open recordings',e);render();}));
$('reconnect').addEventListener('click',()=>run(()=>invoke('reconnect')).catch(()=>{}));
$('unregister').addEventListener('click',async()=>{
  if(!await ask(t('UNREGISTER_CONFIRM')))return;
  act('unregister','','',selected);
});
$('settings-button').addEventListener('click',openSettings);
$('close-settings').addEventListener('click',()=>{$('password').value='';$('configuration').close();});
$('configuration').addEventListener('cancel',e=>{if(busy)e.preventDefault();else $('password').value='';});
$('transport').addEventListener('change',()=>{
  // 既定のポートを使っているときだけ、方式に合わせて入れ替える。
  const tls=$('transport').value==='tls',port=$('port');
  if(port.value.trim()===(tls?'5060':'5061'))port.value=tls?'5061':'5060';
});
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
for(const n of BUTTON_INDEXES)$('button_'+n+'_kind').addEventListener('change',()=>syncButtonRow(n));
$('choose-ca').addEventListener('click',async()=>{
  try{const chosen=await invoke('choose_sound_file',{kind:'certificate'});if(chosen)$('ca_file').value=chosen;}
  catch(e){$('settings-error').textContent=t(String(e));$('settings-error').hidden=false;logUi('choose ca file',e);}
});
for(const button of document.querySelectorAll('[data-sound]'))button.addEventListener('click',async()=>{
  try{const chosen=await invoke('choose_sound_file');if(chosen)$('sound_'+button.dataset.sound).value=chosen;}
  catch(e){$('settings-error').textContent=t(String(e));$('settings-error').hidden=false;logUi('choose sound file',e);}
});
$('settings-form').addEventListener('submit',async e=>{
  e.preventDefault();$('settings-error').hidden=true;
  const settings={network_adapter:$('network_adapter').value,sip_port:Number($('sip_port').value),rtp_port:Number($('rtp_port').value),microphone:state.settings.microphone,speaker:state.settings.speaker,microphone_gain:state.settings.microphone_gain||100,speaker_gain:state.settings.speaker_gain||100,auto_record:!!state.settings.auto_record,buttons:BUTTON_INDEXES.map(n=>({title:$('button_'+n+'_title').value.trim(),kind:$('button_'+n+'_kind').value,number:$('button_'+n+'_kind').value==='dnd'?'':$('button_'+n+'_number').value.trim(),transfer:$('button_'+n+'_kind').value==='park'?$('button_'+n+'_transfer').value.trim():'',pickup:['dial','park'].includes($('button_'+n+'_kind').value)?$('button_'+n+'_pickup').value.trim():''})),auto_answer:$('auto_answer').checked,aec:$('aec').checked,aec_delay_ms:Number($('aec_delay_ms').value),register_interval:Number($('register_interval').value),detail_log:$('detail_log').checked,shortcut_window:$('shortcut_window').value.trim(),shortcut_call:$('shortcut_call').value.trim(),incoming_action:$('incoming_action').value,tray_after_call:Number($('tray_after_call').value),language:$('language').value,browser_integration:$('browser_integration').checked,browser_dial_confirm:$('browser_dial_confirm').checked,transport:$('transport').value,media_encryption:$('media_encryption').value,ca_file:$('ca_file').value.trim(),sound_ring:$('sound_ring').value.trim(),sound_ringback:$('sound_ringback').value.trim(),sound_callwaiting:$('sound_callwaiting').value.trim(),sound_busy:$('sound_busy').value.trim(),sound_notfound:$('sound_notfound').value.trim(),sound_error:$('sound_error').value.trim()};
  const account={server:$('server').value.trim(),port:Number($('port').value),extension:$('extension').value.trim(),auth_user:$('auth_user').value.trim(),password:$('password').value};
  try{await run(()=>invoke('save_configuration',{settings,account}));$('password').value='';$('configuration').close();}
  catch(e){$('settings-error').textContent=t(String(e));$('settings-error').hidden=false;}
  finally{account.password='';}
});
async function calibrateAec(careful){
  $('calibration-status').textContent=t(careful?'CALIBRATION_RUNNING_CAREFUL':'CALIBRATION_RUNNING_SIMPLE');
  try{await run(async()=>{const result=await invoke('calibrate_aec',{microphone:state.settings.microphone||'default',speaker:state.settings.speaker||'default',careful});$('aec_delay_ms').value=result.recommended_ms;$('calibration-status').textContent=fill('CALIBRATION_RESULT',t(result.stable?'CALIBRATION_RECOMMENDED':'CALIBRATION_UNSTABLE'),result.recommended_ms,result.samples,result.spread_ms,result.confidence);});}
  catch(e){$('calibration-status').textContent=t(String(e));logUi('aec calibration',e);}
}
$('calibrate-aec').addEventListener('click',()=>calibrateAec(false));
$('calibrate-aec-careful').addEventListener('click',()=>calibrateAec(true));
$('refresh-devices').addEventListener('click',async()=>{
  $('refresh-devices').disabled=true;$('device-status').textContent=t('DEVICES_LOADING');
  try{await invoke('refresh_devices');update(await invoke('snapshot'));$('device-status').textContent=fill('DEVICES_COUNT',state.devices.length);}
  catch(e){$('device-status').textContent=t(String(e));logUi('refresh devices',e);}
  finally{$('refresh-devices').disabled=false;}
});
$('open-sound-control').addEventListener('click',()=>invoke('open_sound_control').catch(e=>{error=String(e);logUi('open sound control',e);render();}));
for(const kind of kinds)$(kind).addEventListener('change',async()=>{
  const previous=state.settings[kind],device=$(kind).value;
  $(kind+'-volume-status').textContent=t('RECONNECTING');
  try{await run(()=>invoke('select_audio_device',{kind,device}));}
  catch{$(kind).value=previous;}
});
for(const digit of '123456789*0#'){const button=document.createElement('button');button.textContent=digit;button.addEventListener('click',()=>{if(current()?.state==='ESTABLISHED'&&!current().held)act('dtmf',current().id,digit);else if(!current())$('target').value+=digit;});$('keypad').append(button);}
$('history-tab').addEventListener('click',()=>{activePanel='history';render();});
$('logs-tab').addEventListener('click',()=>{activePanel='logs';render();});
$('panel-filter').addEventListener('input',()=>{renderHistory();renderLogs();});
$('clear-panel').addEventListener('click',async()=>{
  const history=activePanel==='history';
  if(!await ask(fill('CLEAR_CONFIRM',t(history?'PANEL_HISTORY':'PANEL_LOGS'))))return;
  try{
    await invoke(history?'clear_call_history':'clear_logs');
    if(history){historyRows=[];historySequence=-1;renderHistory();}
    else{logRows=[];logCursor=0;logShown='';$('logs').textContent='';}
  }catch(e){error=String(e);logUi('clear panel',e);render();}
});
$('quit').addEventListener('click',async()=>{if(!state.calls.length||await ask(t('QUIT_CONFIRM')))invoke('quit_app');});
function volumeDevice(kind){return state.running?state[kind+'_id']:state.settings[kind]||'default';}
// The level and the mute are Windows' own for the device; either is set when
// given, and both are read back, so the icons follow what a headset key or
// another program did within a second.
async function refreshVolume(kind,level=null,mute=null){
  const control=volumes[kind];if(!ready||busy||control.pending||control.dragging||control.pointer)return;
  const device=volumeDevice(kind),sequence=++control.sequence;control.pending=true;
  try{const result=await invoke('audio_volume',{kind,device,level,mute});if(sequence!==control.sequence||device!==volumeDevice(kind)||control.dragging)return;$(kind+'-volume').value=result.level;control.available=true;showMute(kind,result.muted);$(kind+'-level').textContent=result.level+'%';const notices=[];if(result.level>100)notices.push(fill('GAIN_APPLIED',result.level+'%'));if(state[kind+'_missing'])notices.push(t('AUDIO_DEVICE_FALLBACK'));if(kind==='microphone'&&state.microphone_fallback)notices.push(t('MICROPHONE_SILENT'));if(result.muted)notices.push(t('MICROPHONE_MUTED'));$(kind+'-volume-status').textContent=notices.join(' / ');$(kind+'-volume-status').classList.toggle('muted',result.muted);}
  catch(e){control.available=false;$(kind+'-level').textContent='—';$(kind+'-volume-status').textContent=kind==='microphone'&&state.microphone_fallback?t('MICROPHONE_SILENT'):t(String(e));logUi('volume '+kind,e);}
  finally{control.pending=false;const next=control.queued;control.queued=null;render();if(next&&next.device===volumeDevice(kind))refreshVolume(kind,next.level,next.mute);}
}
function showMute(kind,muted){
  const control=volumes[kind],button=$(kind+'-mute'),label=t(muted?'AUDIO_UNMUTE':'AUDIO_MUTE');
  control.muted=muted;button.classList.toggle('muted',muted);button.setAttribute('aria-pressed',String(muted));button.title=label;button.setAttribute('aria-label',label);
}
for(const kind of kinds){
  $(kind+'-volume').addEventListener('pointerdown',()=>{volumes[kind].pointer=true;});
  for(const type of ['pointerup','pointercancel'])window.addEventListener(type,()=>{volumes[kind].pointer=false;});
  $(kind+'-volume').addEventListener('input',()=>{const slider=$(kind+'-volume');volumes[kind].dragging=true;if(volumes[kind].pointer&&Math.abs(Number(slider.value)-100)<=3)slider.value=100;$(kind+'-level').textContent=slider.value+'%';});
  $(kind+'-volume').addEventListener('change',()=>{const c=volumes[kind],level=Number($(kind+'-volume').value);c.dragging=false;c.pointer=false;if(c.pending){++c.sequence;c.queued={device:volumeDevice(kind),level,mute:null};}else refreshVolume(kind,level);});
  $(kind+'-mute').addEventListener('click',()=>{const c=volumes[kind],mute=!c.muted;if(c.pending){++c.sequence;c.queued={device:volumeDevice(kind),level:null,mute};}else refreshVolume(kind,null,mute);});
}
async function syncLogs(){
  const sequence=state.log_sequence||0;
  if(sequence<logCursor){logRows=[];logCursor=0;}
  if(sequence===logCursor)return;
  const page=await invoke('read_logs',{after:logCursor});
  logRows=page.from>logCursor?page.entries:logRows.concat(page.entries);
  if(logRows.length>1000)logRows=logRows.slice(-1000);
  logCursor=page.from+page.entries.length;
  renderLogs();
}
async function syncHistory(){
  const sequence=state.history_sequence??0;
  if(sequence===historySequence)return;
  historyRows=await invoke('read_call_history');historySequence=sequence;renderHistory();
}
// A link or a shortcut key does what the answer and hang-up buttons do.
function onLink(command){
  if(command==='ANSWER')answer();
  if(command==='HANGUP')hangup();
}
// The number arrives as the registrar will dial it, and the app has said
// whether to ask first. One the app refused only goes into the dial box,
// beside the error, so that what came can be seen.
async function onDial({target,confirm,refused}){
  if(refused){$('target').value=target;render();return;}
  if(!target)return;
  if(confirm&&!await ask(fill('DIAL_CONFIRM',target)))return;
  // A link can arrive before the account has registered, for instance when the
  // browser started the app; the call waits for that rather than failing.
  const end=Date.now()+20000;
  while(state.registration!=='REGISTER_OK'&&Date.now()<end)await new Promise(done=>setTimeout(done,250));
  dial(target);
}
window.__TAURI__.event.listen('ksip-link',event=>{onLink(String(event.payload||''));});
window.__TAURI__.event.listen('ksip-dial',event=>{onDial(event.payload||{});});
async function poll(){
  try{
    if(!busy){update(await invoke('snapshot'));await syncLogs();await syncHistory();}
  }catch(e){error=String(e);logUi('poll',e);render();}
  setTimeout(poll,300);
}
async function pollVolumes(){await Promise.all(kinds.map(k=>refreshVolume(k)));setTimeout(pollVolumes,1000);}
async function refreshPeak(kind){
  // In the tray nobody sees the meter, and not asking lets the microphone close.
  if(!ready||$('configuration').open||peakPending[kind]||state.window_visible===false)return;peakPending[kind]=true;
  try{const result=await invoke('audio_peak',{kind,device:volumeDevice(kind)}),gain=kind==='microphone'?Math.max(1,(state.settings.microphone_gain||100)/100):1,raw=Math.max(0,Math.min(1,(result.peak||0)*gain)),db=raw>0?20*Math.log10(raw):-60,level=Math.round(Math.max(0,Math.min(100,(db+60)/60*100)));$(kind+'-meter-fill').style.width=level+'%';$(kind+'-meter').setAttribute('aria-valuenow',String(level));}
  catch(e){$(kind+'-meter-fill').style.width='0%';$(kind+'-meter').setAttribute('aria-valuenow','0');logUi('peak '+kind,e);}
  finally{peakPending[kind]=false;}
}
async function pollAudioPeaks(){await Promise.all(kinds.map(refreshPeak));setTimeout(pollAudioPeaks,100);}
render();poll();pollVolumes();pollAudioPeaks();
