// The call history panel: the rows the app keeps, each with its number to copy
// and, when the call was recorded, the recording to play or show.
import {$, invoke, logUi} from './ui.js';
import {t, fill, language} from './i18n.js';
import {state, dialHistory, setError} from './session.js';
import {render} from './phone.js';
import {passesFilter} from './panel.js';

export let historyRows=[],historySequence=-1;
export function clearHistory(){historyRows=[];historySequence=-1;renderHistory();}
export const historyNumber=peer=>(peer||'—').replace(/^sip:/,'').split('@')[0];
export const durationText=seconds=>String(Math.floor(seconds/60)).padStart(2,'0')+':'+String(seconds%60).padStart(2,'0');

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
    catch(e){setError(String(e));logUi(command,e);render();}
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
    }catch(e){setError(String(e));logUi('copy number',e);render();}
  });
  button.addEventListener('dblclick',event=>event.stopPropagation());
  return button;
}
export function renderHistory(){
  const history=historyRows.filter(item=>passesFilter(historyNumber(item.peer),item.name));
  const panel=$('call-history');panel.replaceChildren();panel.className='history'+(history.length?'':' empty');
  if(!history.length){const empty=document.createElement('div');empty.className='history-empty';empty.textContent=t(historyRows.length?'HISTORY_NO_MATCH':'HISTORY_EMPTY');panel.append(empty);return;}
  for(const item of history){
    const row=document.createElement('div');row.className='history-row';row.title=t('HISTORY_DIAL_HINT');row.addEventListener('dblclick',()=>dialHistory(item.peer));
    const direction=document.createElement('strong');direction.textContent=t(item.direction);
    const time=document.createElement('time');time.textContent=new Date(item.ended_at*1000).toLocaleString(language,{month:'numeric',day:'numeric',hour:'2-digit',minute:'2-digit'});
    const cell=document.createElement('div');cell.className='history-peer-cell';
    const peer=document.createElement('span');peer.className='history-peer';const number=historyNumber(item.peer);peer.textContent=number;
    if(item.name){const who=document.createElement('span');who.className='history-name';who.textContent=item.name;cell.append(who);}
    cell.append(peer);
    if(item.peer)cell.append(copyButton(number));
    if(item.recording)cell.append(playButton(item.recording),locationButton(item.recording));
    const meta=document.createElement('span');meta.className='history-meta'+(item.outcome&&item.outcome!=='HISTORY_ELSEWHERE'?' history-outcome':'');meta.textContent=item.outcome?t(item.outcome):durationText(item.duration||0);
    row.append(direction,time,cell,meta);panel.append(row);
  }
}
export async function syncHistory(){
  const sequence=state.history_sequence??0;
  if(sequence===historySequence)return;
  historyRows=await invoke('read_call_history');historySequence=sequence;renderHistory();
}
