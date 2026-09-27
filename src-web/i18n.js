// 文言は locales/ にある。ここは、どの言語の表を使うかを決め、画面と文言をつなぐ。
// 完全一致、なければ言語の主要部分、それでもなければ日本語。表に無いキーは
// 日本語へ、日本語にも無ければキーそのものを出す。画面が空になるよりよい。
export const TEXTS=window.KSIP_TEXTS||{};
export let texts=TEXTS.ja||{messages:{}};
export let language='ja';

export function languageFor(tag){
  const wanted=String(tag||'').trim();
  if(!wanted)return 'ja';
  if(TEXTS[wanted])return wanted;
  // 中国語は国ではなく文字で分かれるので、地域から書体へ寄せる。
  const chinese={'zh':'zh-TW','zh-HK':'zh-TW','zh-MO':'zh-TW','zh-Hant':'zh-TW','zh-SG':'zh-CN','zh-Hans':'zh-CN'};
  if(chinese[wanted]&&TEXTS[chinese[wanted]])return chinese[wanted];
  const primary=wanted.split('-')[0];
  for(const tag of Object.keys(TEXTS))if(tag===primary||tag.split('-')[0]===primary)return tag;
  return TEXTS.ja?'ja':Object.keys(TEXTS)[0]||'ja';
}
// 選んだ言語を日本語の上に重ねる。訳が無いキーは日本語で出る。画面のどこかが
// 空になるより、そこだけ日本語で出るほうがよい。
export function useLanguage(tag){
  language=languageFor(tag);
  const base=TEXTS.ja||{},chosen=TEXTS[language]||base;
  texts={};
  for(const key of new Set([...Object.keys(base),...Object.keys(chosen)])){
    const japanese=base[key],translated=chosen[key];
    texts[key]=japanese&&typeof japanese==='object'?{...japanese,...(translated||{})}:translated??japanese;
  }
  applyText();
}
// 画面の文字は data-i18n で名前だけを持ち、言語を決めた時点で流し込む。
const TEXT_ATTRIBUTES=[['data-i18n-placeholder','placeholder'],['data-i18n-title','title'],['data-i18n-label','aria-label']];
export function applyText(){
  for(const node of document.querySelectorAll('[data-i18n]'))node.textContent=t(node.getAttribute('data-i18n'));
  for(const [source,target] of TEXT_ATTRIBUTES)
    for(const node of document.querySelectorAll('['+source+']'))node.setAttribute(target,t(node.getAttribute(source)));
  document.documentElement.lang=language;
}
// エンジンと本体は「起きたことの名前」だけを返す。値を伴うものはJSONで届く。
// 名前を知らないときは、その名前をそのまま出す（古い通話履歴もこれで読める）。
export function nameOf(value){
  const text=String(value??'');
  if(!text.startsWith('{'))return {code:text,args:[]};
  try{const parsed=JSON.parse(text);return {code:parsed.code||'',args:parsed.args||[]};}
  catch{return {code:text,args:[]};}
}
export const fill=(code,...args)=>t(JSON.stringify({code,args:args.map(String)}));
export function t(value){
  const {code,args}=nameOf(value);
  if(!code)return '';
  const sentence=texts.messages[code];
  if(!sentence)return code;
  return sentence.replace(/\{(\d+)\}/g,(_,index)=>t(args[index]??''));
}
