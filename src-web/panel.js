// The bottom panel: the history and log tabs, and the one box that narrows
// both. Clearing either is in the settings dialog (settings.js).
import {$} from './ui.js';
import {render} from './phone.js';
import {renderHistory} from './history.js';
import {renderLogs} from './logs.js';

export let activePanel='history';
// One box narrows both panels: the history by number, the log by its source
// tag and its words. Empty means everything, as before.
export const filterText=()=>$('panel-filter').value.trim().toLowerCase();
export const passesFilter=(...parts)=>{const wanted=filterText();return !wanted||parts.some(part=>String(part||'').toLowerCase().includes(wanted));};

export function init(){
  $('history-tab').addEventListener('click',()=>{activePanel='history';render();});
  $('logs-tab').addEventListener('click',()=>{activePanel='logs';render();});
  $('panel-filter').addEventListener('input',()=>{renderHistory();renderLogs();});
}
