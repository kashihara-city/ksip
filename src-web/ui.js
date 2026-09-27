// The page's way to the app and to the person: the invoke bridge, the log the
// page has no other way to write, and the one question at a time it asks.
import {nameOf} from './i18n.js';

export const $=id=>document.getElementById(id);
export const invoke=(command,args={})=>window.__TAURI__.core.invoke(command,args);

// The web view keeps no log of its own, so failures go to the app log. The same
// text repeats at most once a minute per place: a failing poll runs three times
// a second and must not fill the log.
const loggedUi={};
export function logUi(where,detail){
  // The values of a message go too: the Windows error behind AUDIO_DEVICE_FAILED is what a report needs.
  const named=nameOf(detail&&detail.message?detail.message:detail??''),text=where+': '+named.code+named.args.map(a=>' '+nameOf(a).code).join(''),now=Date.now(),last=loggedUi[where];
  if(last&&last.text===text&&now-last.at<60000)return;
  loggedUi[where]={text,at:now};
  invoke('log_ui',{text}).catch(()=>{});
}
window.addEventListener('error',e=>logUi('script error',(e.message||'')+' '+(e.filename||'')+':'+(e.lineno||0)));
window.addEventListener('unhandledrejection',e=>logUi('unhandled rejection',e.reason));

// One question at a time: a request that arrives while another is on screen
// waits for that answer, so that one OK never answers two, and the text on
// screen is always the request being decided.
let asking=Promise.resolve(false);
export function ask(message){
  const turn=()=>askNow(message);
  asking=asking.then(turn,turn);
  return asking;
}
function askNow(message){
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
