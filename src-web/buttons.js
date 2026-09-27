// The custom buttons: the six on the phone and the panel beside it, drawn from
// the settings, and what each kind does when pressed.
import {$, invoke, logUi} from './ui.js';
import {t, fill, texts} from './i18n.js';
import {state, busy, callAt, current, configuredButtons, watchState, act, dial, setError} from './session.js';
import {render} from './phone.js';

// The button rows of the settings are one template stamped out: six for the
// phone, the rest for the panel beside it.
export const BUTTON_MAIN=6,BUTTON_COUNT=30,BUTTON_INDEXES=Array.from({length:BUTTON_COUNT},(_,i)=>i+1);
// Stamped before the wording is applied, so that it reaches them too.
export function buildButtonSets(){
  const html=$('button-set-template').innerHTML;
  for(const n of BUTTON_INDEXES)$(n<=BUTTON_MAIN?'button-sets':'extended-sets').insertAdjacentHTML('beforeend',html.replaceAll('button_N_','button_'+n+'_').replace('>N<','>'+n+'<'));
}
// A switch on the phone itself has a name of its own in the window's language,
// used while the title is left empty; the other kinds fall back to the number.
export const defaultTitle=kind=>kind==='dnd'?t('BUTTON_TITLE_DND'):kind==='mwi'?t('BUTTON_TITLE_MWI'):'';
export const buttonTitle=b=>b.title||defaultTitle(b.kind)||b.number;

// The buttons are drawn from the settings: only the configured ones, in
// order, each keeping its own id so that a test can find it by position.
// The phone's six buttons and the panel's are drawn the same way; the panel
// and the wider window only exist while a panel button is set.
export function renderButtons(c){
  const all=configuredButtons(),extended=all.filter(b=>b.index>BUTTON_MAIN);
  document.body.classList.toggle('extended',extended.length>0);
  renderButtonBox($('custom-actions'),all.filter(b=>b.index<=BUTTON_MAIN),c);
  renderButtonBox($('extended-actions'),extended,c);
}
function renderButtonBox(box,configured,c){
  box.hidden=!configured.length;
  const key=JSON.stringify(configured);
  if(box.dataset.key!==key){
    box.replaceChildren(...configured.map(b=>{const button=document.createElement('button');button.type='button';button.id='custom-'+b.index;button.className='custom-'+b.kind;const title=document.createElement('strong');const status=document.createElement('small');status.id='custom-'+b.index+'-status';button.append(title,status);button.addEventListener('click',()=>useButton(b.index));return button;}));
    box.dataset.key=key;
  }
  const active=c?.state==='ESTABLISHED'&&!c.held,freeLine=[1,2].some(line=>!callAt(line)),registered=state.registration==='REGISTER_OK';
  for(const b of configured){
    const button=$('custom-'+b.index),watched=b.kind==='dial'||b.kind==='park',s=watched?watchState(b.number):'';
    let status,enabled,needsPhone=true;
    if(b.kind==='transfer'){status=fill('BUTTON_TRANSFER_TO',b.number);enabled=active;}
    else if(b.kind==='dial'){status=b.number+' · '+(texts.park[s]||texts.park.UNKNOWN);enabled=freeLine;}
    else if(b.kind==='speed'){status=b.number;enabled=freeLine;}
    else if(b.kind==='open'){status=b.number;enabled=true;needsPhone=false;}
    else if(b.kind==='dnd'){status=t(state.dnd?'DND_ON_STATUS':'DND_OFF_STATUS');enabled=true;}
    else if(b.kind==='mwi'){const m=state.mwi||{};status=m.new>0?fill('MWI_NEW',m.new):t('MWI_NONE');enabled=freeLine;}
    else{status=b.number+' · '+(texts.park[s]||texts.park.UNKNOWN);enabled=s!=='UNKNOWN'&&(active?s==='IDLE':s==='INUSE'&&freeLine);}
    button.firstChild.textContent=buttonTitle(b);
    // A button's children are presentational to assistive technology, so its
    // name carries the status as well as the title.
    button.setAttribute('aria-label',buttonTitle(b)+' '+status);
    $('custom-'+b.index+'-status').textContent=status;
    button.classList.toggle('occupied',watched&&s==='INUSE');
    button.classList.toggle('dnd',b.kind==='dnd'&&!!state.dnd);
    button.classList.toggle('mwi-new',b.kind==='mwi'&&(state.mwi?.new||0)>0);
    button.disabled=busy||!enabled||(needsPhone&&(!registered||!!state.transfer.pending));
  }
}
async function useButton(n){
  const b=configuredButtons().find(button=>button.index===n);
  if(!b)return;
  if(b.kind==='open'){
    try{await invoke('open_link',{url:b.number});}
    catch(e){setError(String(e));logUi('open link',e);render();}
    return;
  }
  if(b.kind==='dnd'){await act('dnd','',state.dnd?'off':'on');return;}
  // A voicemail button and a dial without BLF simply call their number.
  if(b.kind==='mwi'||b.kind==='speed'){await dial(b.number);return;}
  const c=current(),active=c?.state==='ESTABLISHED'&&!c.held,s=b.kind==='transfer'?'':watchState(b.number);
  if(b.kind==='transfer'){if(active)await act('blind_transfer',c.id,b.number);return;}
  if(b.kind==='park'&&active&&s==='IDLE'){await act('blind_transfer',c.id,b.transfer||b.number);return;}
  // A watched number in use is picked up rather than called: the pickup
  // target, which is the number itself unless the button says otherwise.
  if(b.kind==='dial'||(b.kind==='park'&&s==='INUSE'))await dial(s==='INUSE'?b.pickup||b.number:b.number);
}
