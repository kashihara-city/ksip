// The page starts here: the parts are wired in order, the wording is applied,
// and the polling that keeps the window in step with the phone begins.
//
//   ui.js        the invoke bridge, the page's log, the one question at a time
//   i18n.js      which language, and the words for a name
//   session.js   the last snapshot, the selected line, and every operation
//   phone.js     the header, the lines, the call buttons, the footer
//   buttons.js   the custom buttons on the phone and in the panel
//   history.js   the call history panel
//   logs.js      the log panel
//   panel.js     the tabs, the filter box and the clear button
//   audio.js     the devices, their volume and mute, the level meters
//   settings.js  the settings dialog
//   links.js     browser links and shortcut keys arriving from the app
import {invoke, logUi} from './ui.js';
import {useLanguage} from './i18n.js';
import {update, setError} from './session.js';
import {render, init as initPhone} from './phone.js';
import {buildButtonSets} from './buttons.js';
import {syncHistory} from './history.js';
import {syncLogs, init as initLogs} from './logs.js';
import {init as initPanel} from './panel.js';
import {pollVolumes, pollAudioPeaks, init as initAudio} from './audio.js';
import {init as initSettings} from './settings.js';
import {init as initLinks} from './links.js';

// The button rows of the settings are stamped out before the wording is
// applied, so that it reaches them too.
buildButtonSets();
useLanguage(navigator.language);
initPhone();
initLogs();
initPanel();
initAudio();
initSettings();
initLinks();

// The snapshot is read whether or not an operation is under way: the phone
// goes on (a call comes in, ends, the engine reports) while the page waits
// for an answer, and the page shows it. What `busy` keeps back is a second
// operation, not the looking.
async function poll(){
  try{update(await invoke('snapshot'));await syncLogs();await syncHistory();fitStartHeight();}
  catch(e){setError(String(e));logUi('poll',e);render();}
  setTimeout(poll,300);
}
// The window's height at the start, from this first layout with the phone's
// state and the history in it: down to the history's second row, which the
// custom buttons and the notices above it move. Asked once; the app then
// leaves the height to the person. With no history yet, a row is 53px.
let startFitted=false;
function fitStartHeight(){
  if(startFitted)return;startFitted=true;
  const main=document.querySelector('main'),panel=document.querySelector('.bottom-panel');
  const tabs=document.querySelector('.panel-tabs').getBoundingClientRect().height;
  const row=document.querySelector('.history-row')?.getBoundingClientRect().height||53;
  const top=panel.getBoundingClientRect().top+main.scrollTop;
  invoke('fit_start_height',{height:top+tabs+row*2+6}).catch(e=>logUi('fit start height',e));
}
render();poll();pollVolumes();pollAudioPeaks();
