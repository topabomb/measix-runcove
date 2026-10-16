'use strict';
// Prefer the work area over navigation history; include role-less rich editors.
const domExpression = `(()=>{
  const visible=e=>{const r=e.getBoundingClientRect(),s=getComputedStyle(e);return r.width>0&&r.height>0&&s.visibility!=='hidden'&&s.display!=='none'&&r.bottom>0&&r.right>0&&r.top<innerHeight&&r.left<innerWidth};
  const dialogs=[...document.querySelectorAll('[role="dialog"],dialog[open]')].filter(visible);
  const root=dialogs.at(-1)||document.querySelector('main,[role="main"]')||document.body;
  const selector='a,button,input,textarea,select,[role="button"],[role="textbox"],[contenteditable]:not([contenteditable="false"]),h1,h2';
  const primary=[...(root?.querySelectorAll(selector)||[])].filter(visible);
  const rest=[...document.querySelectorAll(selector)].filter(e=>visible(e)&&!primary.includes(e));
  const nodes=[...primary,...(root===document.body?rest:rest.slice(0,8))],text=root?.innerText||'';
  return {title:document.title,url:location.href.slice(0,1000),readyState:document.readyState,region:dialogs.length?'dialog':root===document.body?'body':'main',
    controls:nodes.slice(0,60).map(e=>({tag:e.tagName,role:e.getAttribute('role'),name:(e.getAttribute('aria-label')||e.getAttribute('placeholder')||e.innerText||'').slice(0,120),editable:e.isContentEditable||['INPUT','TEXTAREA','SELECT'].includes(e.tagName),value:('value'in e?String(e.value):e.isContentEditable?e.innerText:undefined)?.slice(0,200),href:e.tagName==='A'?e.href.slice(0,300):undefined,disabled:!!e.disabled})),text:text.slice(0,4000),truncated:nodes.length>60||text.length>4000};
})()`;
function compactAX(nodes){
  const map=new Map(nodes.map(n=>[n.nodeId,n]));
  const roots=nodes.filter(n=>!n.ignored&&['dialog','main'].includes(n.role?.value));
  const picked=roots.filter(n=>n.role.value==='dialog');
  const lines=[];let size=0,truncated=false;
  function visit(n,depth=0){if(!n||size>=6000||lines.length>=100){truncated=true;return;}
    const role=n.role?.value,name=String(n.name?.value||'').slice(0,180);
    if(!n.ignored&&!['generic','none','InlineTextBox'].includes(role)){
      const line=' '.repeat(Math.min(depth,5))+role+(name?' '+JSON.stringify(name):'');
      if(size+line.length>6000){truncated=true;return;}lines.push(line);size+=line.length+1;
    }
    for(const child of n.childIds||[])visit(map.get(child),depth+1);
  }
  for(const n of picked.length?picked:roots.length?roots:nodes.slice(0,1))visit(n);
  return {text:lines.join('\n'),truncated};
}
module.exports={domExpression,compactAX};
