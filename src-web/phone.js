// The phone itself: the header that says who this phone is and how it is,
// the two lines, the dial box and the call buttons, the notices, the
// recording switch and the footer with the audio processing. render() draws
// all of it from the last snapshot.
import {$, invoke, logUi, ask} from './ui.js';
import {t, fill, texts, language} from './i18n.js';
import {state, ready, busy, selected, error, callAt, current, peerNumber, run, act, dial, answer, hangup, setSelected, setError, managed, sendDigit} from './session.js';
import {renderButtons, editing, setEditing} from './buttons.js';
import {closeSettingsForCall} from './settings.js';
import {renderLogs} from './logs.js';
import {activePanel} from './panel.js';
import {kinds, volumes} from './audio.js';

// A notice (the transfer's outcome, the banner) is shown for a while after it
// is set, then goes; an operation takes it away at once. A transfer still in
// progress keeps its notice. Without this a notice stayed through unrelated work.
// The outcome is told apart by its count as well as its words, so the same
// outcome set again (back to the held call, twice) shows again.
const NOTICE_MS=5000;
let noticeKey='',noticeSince=0,bannerText='',bannerSince=0;

// The caller's name, when the call came with one, in front of the number.
const peerLabel=c=>{const number=peerNumber(c);return number?(c.name?c.name+' '+number:number):'—';};
const status=c=>c?(c.held?texts.callHeld:texts.callState[c.state]||c.state):texts.callIdle;
const metric=(value,unit,digits=1)=>Number.isFinite(value)?value.toFixed(digits)+unit:'—';
// The words for each noise suppression strength, shared by the settings and the footer.
const NOISE_LEVEL_NAMES={low:'SETTINGS_NS_LOW',moderate:'SETTINGS_NS_MODERATE',high:'SETTINGS_NS_HIGH',very_high:'SETTINGS_NS_VERY_HIGH'};
const count=value=>Number.isFinite(value)?value.toLocaleString(language):'—';
const codecNames={opus:'Opus',G722:'G.722',PCMU:'G.711 μ-law',PCMA:'G.711 A-law'};
const encryptionNames={'srtp-mand':'SRTP（SDES）',srtp:'SRTP（OSRTP）',dtls_srtp:'SRTP（DTLS）'};
// OSRTP falls back to plain RTP by design; that is the one case the footer says in red.
function fellBack(call){return !!(call&&call.codec&&!call.secure&&state.media_encryption==='srtp');}
function codecName(reported){
  const [name,rate]=reported.split(' ');
  const hz=parseInt(rate,10);
  return (codecNames[name]||name)+(Number.isFinite(hz)?' '+hz/1000+' kHz':'');
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
export function render({clearNotices=false}={}){
  if(clearNotices){noticeSince=0;bannerSince=0;}
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
  // As the engine says it runs, like the transport: not the saved setting.
  $('detail-log-active').hidden=!(state.running&&state.detail_log_active);
  // In a call (connected to the other end, on hold too) the settings and
  // the button editing cannot be entered; while a call only rings they can.
  // A call that starts closes them, by whatever way it started (a key, the
  // automatic answer, a link), without saving, and says so.
  const talking=state.calls.some(call=>call.state==='ESTABLISHED');
  if(talking){
    const closed=closeSettingsForCall();
    const ended=editing;
    if(ended)setEditing(false);
    if(closed||ended)setError(t('SETTINGS_CLOSED_FOR_CALL'));
  }
  $('settings-button').disabled=!ready||busy||talking;
  $('edit-buttons').textContent=t(editing?'BUTTONS_EDIT_END':'BUTTONS_EDIT');$('edit-buttons').setAttribute('aria-pressed',String(editing));$('edit-buttons').classList.toggle('on',editing);
  $('edit-buttons').disabled=editing?busy:!ready||busy||talking;
  // One switch for the connection: to unregister while registered, to
  // connect again otherwise; neither while a call is up or ringing, which
  // either would end, nor while a registration is under way.
  const connecting=['CONNECTING','REGISTERING'].includes(state.registration);
  $('unregister').hidden=!registered;$('reconnect').hidden=registered;
  // Unregistered on purpose, the switch stands out, as the editing switch
  // does while it is on: the phone is off until it is pressed.
  $('reconnect').classList.toggle('on',state.registration==='UNREGISTERED');
  $('reconnect').disabled=!ready||busy||state.calls.length>0||!state.account.has_password||connecting;
  // Unregistering is for a meeting: it stops calls arriving without closing KSIP.
  $('unregister').disabled=!ready||busy||state.calls.length>0||!state.running||state.registration==='UNREGISTERED';
  for(let n=1;n<=2;n++){
    const c=callAt(n),seconds=c?.duration||0;
    $('line-'+n).className='line'+(selected===n?' selected':'')+(c?.state==='INCOMING'?' incoming':'')+' call-'+(c?(c.held?'held':String(c.state).toLowerCase()):'idle');
    $('line-'+n).setAttribute('aria-pressed',String(selected===n));$('line-'+n).disabled=busy||!!state.transfer.pending;
    $('line-'+n+'-status').textContent=status(c);$('line-'+n+'-peer').textContent=peerLabel(c);
    $('line-'+n+'-duration').textContent=String(Math.floor(seconds/60)).padStart(2,'0')+':'+String(seconds%60).padStart(2,'0');
  }
  const c=current();
  $('target').disabled=busy||!!c;$('clear-target').disabled=$('target').disabled;
  $('dial').disabled=busy||!!c||state.registration!=='REGISTER_OK'||!!state.transfer.pending;
  $('answer').disabled=busy||c?.state!=='INCOMING'||!!state.transfer.pending;
  $('hangup').disabled=busy||!c;
  $('hold').disabled=busy||c?.state!=='ESTABLISHED'||!!state.transfer.pending;
  $('hold').textContent=t(c?.held?'HOLD_RESUME':'HOLD');
  renderButtons(c);
  $('transfer').disabled=busy||!!state.transfer.pending||![callAt(1),callAt(2)].every(c=>c?.state==='ESTABLISHED');
  const outcome=state.transfer.outcome||'',outcomeText=t(outcome),outcomeKey=outcome+'#'+(state.transfer.outcome_seq||0);
  if(outcomeKey!==noticeKey){noticeKey=outcomeKey;noticeSince=Date.now();}
  const showOutcome=!!outcomeText&&(!!state.transfer.pending||Date.now()-noticeSince<NOTICE_MS);
  $('transfer-status').className='hint'+(showOutcome?' '+outcome.toLowerCase().replace(/_/g,'-'):'');
  $('transfer-status').textContent=showOutcome?outcomeText:'';
  // The automatic recording a policy fixes is not switched here.
  $('record').disabled=!ready||busy||managed.has('auto_record');$('record').title=managed.has('auto_record')?t('SETTINGS_MANAGED_FIELD'):'';$('record').textContent=t(state.settings.auto_record?'RECORD_ON':'RECORD_OFF');$('record').classList.toggle('active',!!state.settings.auto_record);$('record').setAttribute('aria-pressed',String(!!state.settings.auto_record));
  $('record-status').classList.toggle('recording',!!state.recording);$('record-status').textContent=t(state.recording?'RECORD_RECORDING':state.converting?'RECORDING_CONVERTING':state.settings.auto_record?'RECORD_AUTO_ON':'RECORD_AUTO_OFF');
  renderFooter();
  const banner=t(error)||t(state.error)||'';
  if(banner!==bannerText){bannerText=banner;bannerSince=Date.now();}
  const showBanner=!!banner&&Date.now()-bannerSince<NOTICE_MS;
  $('error').textContent=showBanner?banner:'';$('error').hidden=!showBanner;
  $('call-history').hidden=activePanel!=='history';$('logs').hidden=activePanel!=='logs';
  $('history-tab').classList.toggle('active',activePanel==='history');$('logs-tab').classList.toggle('active',activePanel==='logs');
  $('history-tab').setAttribute('aria-selected',String(activePanel==='history'));$('logs-tab').setAttribute('aria-selected',String(activePanel==='logs'));
  renderLogs();
  for(const kind of kinds){$(kind+'-volume').disabled=busy||!volumes[kind].available;$(kind+'-mute').disabled=busy||!volumes[kind].available;$(kind).disabled=busy||state.calls.length>0||!state.account.has_password;}
  $('refresh-devices').disabled=busy||state.calls.length>0;
  $('save-settings').disabled=busy;$('close-settings').disabled=busy;for(const id of ['calibrate-aec','calibrate-aec-careful'])$(id).disabled=busy||state.calls.length>0;
}
// The footer names every part of the audio processing and whether it is on;
// the numbers below it are the AEC's, then the levels, then the AGC's report.
function renderFooter(){
  const processing=state.settings,noiseOn=!!NOISE_LEVEL_NAMES[processing.noise_suppression],processingOn=!!processing.aec||!!processing.high_pass||noiseOn||!!processing.agc;
  $('processing-labels').textContent=['AEC '+(processing.aec?'ON':'OFF'),'HPF '+(processing.high_pass?'ON':'OFF'),'NS '+(noiseOn?t(NOISE_LEVEL_NAMES[processing.noise_suppression]):'OFF'),'AGC '+(processing.agc?'ON':'OFF')].join(' · ');
  // A codec and an encryption belong to a call, so outside one this stays empty.
  $('codec-label').textContent=callSummary(current());$('codec-label').classList.toggle('unencrypted',fellBack(current()));
  const metrics=state.audio_processing_stats;
  const showMetrics=processingOn&&!!state.aec_active&&state.calls.some(call=>call.state==='ESTABLISHED')&&!!metrics;
  $('aec-metrics').hidden=!showMetrics;
  $('aec-state').hidden=!showMetrics||!processing.aec;
  if(!showMetrics){
    for(const id of ['aec-core','aec-delay','aec-levels','aec-flow','aec-agc'])$(id).textContent='';
    return;
  }
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
  if(!processing.aec){$('aec-core').textContent='';$('aec-delay').textContent='';}
  $('aec-agc').textContent=!processing.agc?'':Number.isFinite(metrics.agc_gain_db)?fill('AGC_REPORT',metric(metrics.agc_gain_db,' dB'),metric(metrics.agc_speech_level_dbfs,' dBFS',0),metric(metrics.agc_noise_level_dbfs,' dBFS',0),metric(metrics.agc_headroom_db,' dB',0)):t('AGC_WAITING');
}
// Empties the number box and puts the cursor there, when it can be typed in.
function clearTarget(){$('target').value='';if(!$('target').disabled)$('target').focus();}
export function init(){
  for(let n=1;n<=2;n++)$('line-'+n).addEventListener('click',async()=>{
    const before=selected;
    try{await run(async()=>{if(state.running)await invoke('action',{name:'select',id:callAt(n)?.id||'',value:'',line:n});setSelected(n);});}catch{}
    // Moving to the other line is, as often as not, getting ready to transfer:
    // the number box is emptied as the clear button does, ready for a number.
    // Whether the line moved is what counts, not how the refresh after it went.
    if(selected!==before)clearTarget();
  });
  $('dial-form').addEventListener('submit',e=>{e.preventDefault();dial($('target').value.trim());});
  // The number stays in the box after a call (to see what was dialled); this
  // empties it and puts the cursor there for the next one.
  $('clear-target').addEventListener('click',clearTarget);
  $('answer').addEventListener('click',()=>answer());
  $('hangup').addEventListener('click',()=>hangup());
  $('hold').addEventListener('click',()=>act(current()?.held?'resume':'hold'));
  $('transfer').addEventListener('click',()=>act('transfer',callAt(1)?.id||'',callAt(2)?.id||'',1));
  $('record').addEventListener('click',()=>act('auto_record','',state.settings.auto_record?'off':'on'));
  $('open-recordings').addEventListener('click',()=>invoke('open_recordings').catch(e=>{setError(String(e));logUi('open recordings',e);render();}));
  $('reconnect').addEventListener('click',()=>run(()=>invoke('reconnect')).catch(()=>{}));
  $('edit-buttons').addEventListener('click',()=>setEditing(!editing));
  $('unregister').addEventListener('click',async()=>{
    if(!await ask(t('UNREGISTER_CONFIRM')))return;
    act('unregister','','',selected);
  });
  for(const digit of '123456789*0#'){const button=document.createElement('button');button.textContent=digit;button.addEventListener('click',()=>{const c=current();if(c?.state==='ESTABLISHED'&&!c.held)sendDigit(c,digit);else if(!c)$('target').value+=digit;});$('keypad').append(button);}
  $('quit').addEventListener('click',async()=>{if(!state.calls.length||await ask(t('QUIT_CONFIRM')))invoke('quit_app');});
}
