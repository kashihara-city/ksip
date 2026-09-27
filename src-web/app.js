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
import {busy, update, setError} from './session.js';
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

async function poll(){
  try{
    if(!busy){update(await invoke('snapshot'));await syncLogs();await syncHistory();}
  }catch(e){setError(String(e));logUi('poll',e);render();}
  setTimeout(poll,300);
}
render();poll();pollVolumes();pollAudioPeaks();
