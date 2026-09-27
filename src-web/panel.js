// The bottom panel: the history and log tabs, the one box that narrows both,
// and the button that clears the one being shown.
import {$, invoke, logUi, ask} from './ui.js';
import {t, fill} from './i18n.js';
import {setError} from './session.js';
import {render} from './phone.js';
import {renderHistory, clearHistory} from './history.js';
import {renderLogs, clearLogs} from './logs.js';

export let activePanel='history';
// One box narrows both panels: the history by number, the log by its source
// tag and its words. Empty means everything, as before.
export const filterText=()=>$('panel-filter').value.trim().toLowerCase();
export const passesFilter=(...parts)=>{const wanted=filterText();return !wanted||parts.some(part=>String(part||'').toLowerCase().includes(wanted));};

export function init(){
  $('history-tab').addEventListener('click',()=>{activePanel='history';render();});
  $('logs-tab').addEventListener('click',()=>{activePanel='logs';render();});
  $('panel-filter').addEventListener('input',()=>{renderHistory();renderLogs();});
  $('clear-panel').addEventListener('click',async()=>{
    const history=activePanel==='history';
    if(!await ask(fill('CLEAR_CONFIRM',t(history?'PANEL_HISTORY':'PANEL_LOGS'))))return;
    try{
      await invoke(history?'clear_call_history':'clear_logs');
      if(history)clearHistory();
      else clearLogs();
    }catch(e){setError(String(e));logUi('clear panel',e);render();}
  });
}
