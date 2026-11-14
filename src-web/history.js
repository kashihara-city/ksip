// The call history panel: the rows the app keeps, each with its number to
// copy beside it, and after the numbers the buttons for the call: the call
// back and, when the call was recorded, the recording to play or show.
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
  return recordingButton('history-icon history-play',t('HISTORY_PLAY'),'<path d="M4.5 2.6v10.8L13.5 8z"/>','open_recording',name);
}
// The same file shown in Explorer, selected, as a download bar offers.
function locationButton(name){
  return recordingButton('history-icon history-location',t('HISTORY_LOCATION'),'<path d="M1.8 4.6A1.3 1.3 0 0 1 3.1 3.3h3l1.5 1.5h5.3a1.3 1.3 0 0 1 1.3 1.3v6.1a1.3 1.3 0 0 1-1.3 1.3H3.1a1.3 1.3 0 0 1-1.3-1.3z"/>','open_recording_location',name);
}
// An icon button in a history row: the wording lives in the title, and in
// the name read out (`label`, which says the number where there is one).
function iconButton(className,title,label,icon){
  const button=document.createElement('button');
  button.type='button';button.className=className;
  button.title=title;button.setAttribute('aria-label',label);
  button.innerHTML='<svg viewBox="0 0 16 16" aria-hidden="true">'+icon+'</svg>';
  return button;
}
function recordingButton(className,label,icon,command,name){
  const button=iconButton(className,label,label,icon);
  button.addEventListener('click',async()=>{
    try{await invoke(command,{name});}
    catch(e){setError(String(e));logUi(command,e);render();}
  });
  return button;
}
function copyButton(number){
  const button=iconButton('history-copy',t('COPY_NUMBER'),fill('COPY_NUMBER_LABEL',number),
    '<rect x="5.2" y="5.2" width="8" height="9" rx="1.4"/><path d="M10.8 3.2H3.6a1.4 1.4 0 0 0-1.4 1.4v7.2"/>');
  button.addEventListener('click',async()=>{
    try{
      await invoke('copy_text',{text:number});
      button.title=t('COPY_DONE');
      setTimeout(()=>{button.title=t('COPY_NUMBER');},1500);
    }catch(e){setError(String(e));logUi('copy number',e);render();}
  });
  return button;
}
// The call back: the row's far end is dialled as the dial box would dial it
// (session.js, dialHistory), and what keeps a call from starting is said there.
function dialButton(peer,number){
  const button=iconButton('history-icon history-dial',t('HISTORY_DIAL'),fill('HISTORY_DIAL_LABEL',number),
    '<path d="M3.3 2.3h2.4l1.1 2.9-1.5 1.1a7.2 7.2 0 0 0 4.4 4.4l1.1-1.5 2.9 1.1v2.4a1.2 1.2 0 0 1-1.3 1.2A10.9 10.9 0 0 1 2.1 3.6 1.2 1.2 0 0 1 3.3 2.3z"/>');
  button.addEventListener('click',()=>dialHistory(peer));
  return button;
}
export function renderHistory(){
  const history=historyRows.filter(item=>passesFilter(historyNumber(item.peer),item.name));
  const panel=$('call-history');panel.replaceChildren();panel.className='history'+(history.length?'':' empty');
  if(!history.length){const empty=document.createElement('div');empty.className='history-empty';empty.textContent=t(historyRows.length?'HISTORY_NO_MATCH':'HISTORY_EMPTY');panel.append(empty);return;}
  for(const item of history){
    const row=document.createElement('div');row.className='history-row';
    const direction=document.createElement('strong');direction.textContent=t(item.direction);
    const time=document.createElement('time');time.textContent=new Date(item.ended_at*1000).toLocaleString(language,{month:'numeric',day:'numeric',hour:'2-digit',minute:'2-digit'});
    const cell=document.createElement('div');cell.className='history-peer-cell';
    const peer=document.createElement('span');peer.className='history-peer';const number=historyNumber(item.peer);peer.textContent=number;
    if(item.name){const who=document.createElement('span');who.className='history-name';who.textContent=item.name;cell.append(who);}
    cell.append(peer);
    // Copying is about the number, and stands right after it; the buttons
    // for the call follow the numbers' column, the call back first so that
    // it sits in the same place in every row, whatever else the row has.
    if(item.peer)cell.append(copyButton(number));
    const actions=document.createElement('span');actions.className='history-actions';
    if(item.peer)actions.append(dialButton(item.peer,number));
    if(item.recording)actions.append(playButton(item.recording),locationButton(item.recording));
    const meta=document.createElement('span');meta.className='history-meta'+(item.outcome&&item.outcome!=='HISTORY_ELSEWHERE'?' history-outcome':'');meta.textContent=item.outcome?t(item.outcome):durationText(item.duration||0);
    row.append(direction,time,cell,actions,meta);panel.append(row);
  }
}
export async function syncHistory(){
  const sequence=state.history_sequence??0;
  if(sequence===historySequence)return;
  historyRows=await invoke('read_call_history');historySequence=sequence;renderHistory();
}
