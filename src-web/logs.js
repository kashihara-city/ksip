// The log panel: the lines the app hands over, shown newest first, and left
// alone while the person is selecting in it.
import {$, invoke} from './ui.js';
import {t} from './i18n.js';
import {state} from './session.js';
import {filterText, passesFilter, activePanel} from './panel.js';

export let logRows=[],logCursor=0;
let logShown='';
export function clearLogs(){logRows=[];logCursor=0;logShown='';$('logs').textContent='';}
const two=value=>String(value).padStart(2,'0');
function logTime(iso){const at=new Date(iso);return isNaN(at)?iso:(at.getMonth()+1)+'/'+at.getDate()+' '+at.getHours()+':'+two(at.getMinutes())+':'+two(at.getSeconds());}
const logMessage=row=>row.code?t(row.code&&row.args?.length?JSON.stringify({code:row.code,args:row.args}):row.code):row.text;
function logText(row){const body=logMessage(row);return body?logTime(row.time)+' ['+row.src+'] '+body:'';}
// Newest first, as the call history is shown.
const logBody=()=>logRows.filter(row=>passesFilter(row.src,logMessage(row))).map(logText).reverse().join('\n');
// The log tab is left alone while the person is in it, selecting lines to
// copy: rewriting the text would drop the selection. Lines keep arriving
// meanwhile and are shown the moment the tab is left.
function logPaused(){
  const logs=$('logs'),selection=document.getSelection();
  return document.activeElement===logs||(!!selection&&!selection.isCollapsed&&logs.contains(selection.anchorNode));
}
export function renderLogs(){
  const paused=activePanel==='logs'&&logPaused();
  $('logs-paused').hidden=!paused;
  $('filter-active').hidden=!filterText();
  if(activePanel!=='logs'||paused)return;
  const body=logBody();
  if(body!==logShown){logShown=body;$('logs').textContent=body;}
}
export async function syncLogs(){
  const sequence=state.log_sequence||0;
  if(sequence<logCursor){logRows=[];logCursor=0;}
  if(sequence===logCursor)return;
  const page=await invoke('read_logs',{after:logCursor});
  logRows=page.from>logCursor?page.entries:logRows.concat(page.entries);
  if(logRows.length>1000)logRows=logRows.slice(-1000);
  logCursor=page.from+page.entries.length;
  renderLogs();
}
export function init(){
  $('logs').addEventListener('blur',renderLogs);
  document.addEventListener('selectionchange',renderLogs);
  // Ctrl+A in the log selects the log, not the whole window.
  $('logs').addEventListener('keydown',e=>{
    if(e.key.toLowerCase()!=='a'||!(e.ctrlKey||e.metaKey))return;
    e.preventDefault();const range=document.createRange();range.selectNodeContents($('logs'));
    const selection=document.getSelection();selection.removeAllRanges();selection.addRange(range);
  });
}
