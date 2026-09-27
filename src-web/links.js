// What arrives from outside the window: a browser link or a shortcut key,
// handed over by the app once the page listens.
import {$, invoke, logUi, ask} from './ui.js';
import {fill} from './i18n.js';
import {state, dial, answer, hangup} from './session.js';
import {render} from './phone.js';

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
// The listeners are in place; links held for the page since it started may come now.
export function init(){
  Promise.all([
    window.__TAURI__.event.listen('ksip-link',event=>{onLink(String(event.payload||''));}),
    window.__TAURI__.event.listen('ksip-dial',event=>{onDial(event.payload||{});}),
  ]).then(()=>invoke('ui_ready')).catch(e=>logUi('ui ready',e));
}
