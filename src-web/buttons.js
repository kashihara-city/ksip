// The custom buttons: the six on the phone and the panel beside it, drawn from
// the settings, and what each kind does when pressed.
import {$, invoke, logUi, ask} from './ui.js';
import {t, fill, texts} from './i18n.js';
import {state, busy, callAt, current, configuredButtons, watchState, act, dial, setError, run, managedButton} from './session.js';
import {render} from './phone.js';
import {openButtonSettings} from './settings.js';

// The button rows of the settings are one template stamped out: six for the
// phone, then twenty-four for each of the two panels beside it (7 to 30, and
// 31 to 54), as two expansion units put side by side.
export const BUTTON_MAIN=6,BUTTON_PANEL=30,BUTTON_COUNT=54,BUTTON_INDEXES=Array.from({length:BUTTON_COUNT},(_,i)=>i+1);
// Where each run of buttons is drawn, and how many to a row.
const BOXES=[['custom-actions',1,BUTTON_MAIN,3],['extended-actions',BUTTON_MAIN+1,BUTTON_PANEL,2],['extended-actions-2',BUTTON_PANEL+1,BUTTON_COUNT,2]];
// Stamped before the wording is applied, so that it reaches them too.
export function buildButtonSets(){
  const html=$('button-set-template').innerHTML;
  for(const n of BUTTON_INDEXES)$(n<=BUTTON_MAIN?'button-sets':n<=BUTTON_PANEL?'extended-sets':'extended-sets-2').insertAdjacentHTML('beforeend',html.replaceAll('button_N_','button_'+n+'_').replace('>N<','>'+n+'<'));
}
// A switch on the phone itself has a name of its own in the window's language,
// used while the title is left empty; the other kinds fall back to the number.
export const defaultTitle=kind=>kind==='dnd'?t('BUTTON_TITLE_DND'):kind==='mwi'?t('BUTTON_TITLE_MWI'):'';
export const buttonTitle=b=>b.title||defaultTitle(b.kind)||b.number;

// Button editing: while it is on, a click on the phone does nothing (what is
// not a button is made inert, and dimmed), every one of the fifty-four slots is
// shown, both panels' too, with an edit and (for a set one) a delete icon,
// and a button is moved by dragging it onto another slot. It stays on
// through a call ringing; a call that starts ends it (see phone.js).
export let editing=false;
// What is made inert while editing: everything on the page but the buttons
// and the switch that ends the editing. Dialogs are outside it.
const EDIT_INERT='.header-actions > :not(#edit-buttons), .phone > :not(#custom-actions), .audio, .recording, .bottom-panel';
export function setEditing(on){
  if(editing===on)return;
  editing=on;
  document.body.classList.toggle('editing-buttons',on);
  for(const element of document.querySelectorAll(EDIT_INERT))element.inert=on;
  // The window is widened for the panel's slots while editing, and fitted
  // back to what the buttons need after.
  invoke('set_button_editing',{editing:on}).catch(e=>logUi('button editing',e));
  render();
}
// Every button as the settings hold them, every field there.
function allButtons(){
  return BUTTON_INDEXES.map(n=>({title:'',kind:'',number:'',transfer:'',pickup:'',...((state.settings.buttons||[])[n-1]||{})}));
}
const EMPTY_BUTTON={title:'',kind:'',number:'',transfer:'',pickup:''};
// A delete or a move is saved at once, the buttons alone; a call up or not.
async function saveButtons(buttons){
  try{await run(()=>invoke('save_buttons',{buttons}));}
  catch(e){logUi('save buttons',e);}
}
async function deleteButton(n){
  const b=allButtons()[n-1];
  if(!b.kind||!await ask(fill('BUTTON_DELETE_CONFIRM',buttonTitle(b)||String(n))))return;
  const buttons=allButtons();buttons[n-1]={...EMPTY_BUTTON};
  await saveButtons(buttons);
}
// A button dragged onto another slot moves there; if that slot was set,
// the two change places. Nothing else moves.
async function moveButton(from,to){
  if(from===to)return;
  const buttons=allButtons();
  [buttons[from-1],buttons[to-1]]=[buttons[to-1],buttons[from-1]];
  await saveButtons(buttons);
}
// HTML drag and drop inside the window needs the window's own file drop
// off: on Windows, Tauri's takes every drag otherwise (tauri.conf.json,
// dragDropEnabled false).
const DRAG_TYPE='application/x-ksip-button';
// What the second line of a set slot says while editing: what the button
// names, as the button shows it, without anything that changes with the phone.
function slotNumber(b){
  if(b.kind==='transfer')return fill('BUTTON_TRANSFER_TO',b.number);
  if(b.kind==='dnd')return '';
  return b.number||'';
}
// A button a policy fixes keeps its place and what it does: no tools, not
// dragged, and nothing is dropped on it. The rest move among themselves.
function editSlot(n,b){
  const slot=document.createElement('div'),fixed=managedButton(n);
  slot.id='custom-'+n;slot.dataset.slot=String(n);
  slot.className='button-slot'+(b.kind?' custom-'+b.kind:' empty')+(fixed?' managed':'');
  slot.draggable=!!b.kind&&!fixed;
  // The same two lines as the button itself, in the same size, so that
  // nothing moves when the editing starts: the title, and under it what the
  // button names (without the BLF state, which is the phone's, not the
  // setting's). An empty slot says so, with an empty second line.
  const title=document.createElement('strong');title.textContent=b.kind?buttonTitle(b):t('BUTTON_SLOT_EMPTY');
  const named=document.createElement('small');named.textContent=slotNumber(b)||'\u00a0';
  const tools=document.createElement('span');tools.className='button-tools';
  const place=document.createElement('small');place.className='slot-place';place.textContent='#'+n;
  tools.append(place);
  const tool=(what,label,glyph,act)=>{const button=document.createElement('button');button.type='button';button.id='custom-'+n+'-'+what;button.textContent=glyph;button.title=label;button.setAttribute('aria-label',label+' #'+n);button.addEventListener('click',act);return button;};
  if(fixed){const note=document.createElement('small');note.className='slot-managed';note.textContent=t('SETTINGS_MANAGED_FIELD');tools.append(note);slot.title=t('SETTINGS_MANAGED_FIELD');}
  else{
    tools.append(tool('edit',t('BUTTON_EDIT_ONE'),'✎',()=>openButtonSettings(n)));
    if(b.kind)tools.append(tool('delete',t('BUTTON_DELETE_ONE'),'✕',()=>deleteButton(n)));
  }
  slot.append(title,named,tools);
  slot.addEventListener('dragstart',e=>{e.dataTransfer.setData(DRAG_TYPE,String(n));e.dataTransfer.effectAllowed='move';});
  slot.addEventListener('dragover',e=>{if(!fixed&&e.dataTransfer.types.includes(DRAG_TYPE)){e.preventDefault();e.dataTransfer.dropEffect='move';slot.classList.add('drop-target');}});
  slot.addEventListener('dragleave',()=>slot.classList.remove('drop-target'));
  slot.addEventListener('drop',e=>{
    slot.classList.remove('drop-target');
    const from=Number(e.dataTransfer.getData(DRAG_TYPE));
    if(!from||fixed||managedButton(from))return;
    e.preventDefault();
    moveButton(from,n);
  });
  return slot;
}
function renderEditing(){
  const buttons=allButtons();
  document.body.classList.add('extended','extended-2');
  for(const [id,from,to] of BOXES){
    const box=$(id);
    box.hidden=false;
    const key='edit:'+JSON.stringify(buttons.slice(from-1,to))+':'+t('BUTTON_SLOT_EMPTY')+':'+BUTTON_INDEXES.filter(n=>n>=from&&n<=to&&managedButton(n)).join(',');
    if(box.dataset.key===key)continue;
    box.replaceChildren(...BUTTON_INDEXES.filter(n=>n>=from&&n<=to).map(n=>editSlot(n,buttons[n-1])));
    box.dataset.key=key;
  }
}

// The buttons are drawn from the settings, each in the place of its slot, so
// that a button is where the editing put it: an empty slot before the last
// set one keeps its place, empty; the rows after the last set one are left
// out. Each button keeps its own id so that a test can find it by position.
// The phone's six buttons (three to a row) and each panel's twenty-four (two
// to a row) are drawn the same way. A panel and the wider window only exist
// while one of its buttons is set; the first panel stays, empty, while the
// second has one, so that the second is always in its own place.
export function renderButtons(c){
  if(editing){renderEditing();return;}
  const all=configuredButtons(),second=all.some(b=>b.index>BUTTON_PANEL);
  document.body.classList.toggle('extended',all.some(b=>b.index>BUTTON_MAIN));
  document.body.classList.toggle('extended-2',second);
  for(const [id,from,to,columns] of BOXES)renderButtonBox($(id),from,columns,all.filter(b=>b.index>=from&&b.index<=to),c);
  if(second)$('extended-actions').hidden=false;
}
function renderButtonBox(box,first,columns,configured,c){
  box.hidden=!configured.length;
  const key=JSON.stringify(configured);
  if(box.dataset.key!==key){
    const last=configured.length?configured[configured.length-1].index:first-1;
    const end=first+Math.ceil((last-first+1)/columns)*columns-1;
    const places=[];
    for(let n=first;n<=end;n++){
      const b=configured.find(button=>button.index===n);
      // An empty slot is kept as a slot of the button's own size, unseen: an
      // empty element would have no height, and a row of them would close up.
      if(!b){const gap=document.createElement('div');gap.className='button-slot button-gap';gap.setAttribute('aria-hidden','true');const top=document.createElement('strong'),bottom=document.createElement('small');top.textContent=bottom.textContent='\u00a0';gap.append(top,bottom);places.push(gap);continue;}
      const button=document.createElement('button');button.type='button';button.id='custom-'+b.index;button.className='custom-'+b.kind;const title=document.createElement('strong');const status=document.createElement('small');status.id='custom-'+b.index+'-status';button.append(title,status);button.addEventListener('click',()=>useButton(b.index));places.push(button);
    }
    box.replaceChildren(...places);
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
    try{await invoke('open_link',{index:n,callId:current()?.id??null});}
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
