'use strict';
// Deterministic structured-browser interaction fixtures. No external sites.
const http = require('node:http');
const page = (title, body, script = '') => `<!doctype html><html><head><meta charset=utf-8><title>${title}</title>
<style>body{font:14px sans-serif;margin:20px}nav a{margin-right:10px}.overlay{position:fixed;inset:0;background:rgba(0,0,0,.4);display:flex;align-items:center;justify-content:center}.box{background:#fff;padding:20px}#list li{height:48px}</style></head>
<body>${body}<script>${script}</script></body></html>`;
const record = [];
const pages = {
  '/long': page('Long document', '<button id=top>Top marker</button><div style="height:6000px;background:linear-gradient(white,lightblue)">Tall content</div><iframe src="/frame" title="Long page frame" style="width:600px;height:220px"></iframe><button id=bottom>Bottom marker</button><p id=out></p><div id=end style="height:100px;background:rgb(255,0,0)">End of document</div>', 'bottom.onclick=()=>out.textContent="bottom clicked";window.events={scroll:0,resize:0};addEventListener("scroll",()=>events.scroll++);addEventListener("resize",()=>events.resize++);'),
  '/panels': page('Floating panels', `<h1>Panels</h1><button id=menu>Actions</button><button id=hover>Hover for help</button><button id=pop>Open popover</button><button id=modal>Open nested modal</button><div id=slot></div><div id=out role=status></div><div id=popover popover><p>Popover content</p><button id=confirm>Confirm popover</button></div>`, `menu.onclick=()=>{slot.innerHTML='<div role="menu" aria-label="Actions menu" style="position:fixed;left:100px;top:100px;z-index:10;background:white"><button role="menuitem" id="choose">Choose item</button></div>';document.getElementById('choose').onclick=()=>{out.textContent='item chosen';slot.replaceChildren();};};hover.onmouseenter=()=>{slot.innerHTML='<div role="tooltip">Helpful floating text</div>';};hover.onmouseleave=()=>slot.replaceChildren();pop.onclick=()=>popover.showPopover();confirm.onclick=()=>{out.textContent='popover confirmed';popover.hidePopover();};modal.onclick=()=>{slot.innerHTML='<div role="dialog" aria-modal="true" aria-label="Outer modal" style="position:fixed;inset:0;background:white;z-index:20"><button id="next">Open inner modal</button></div>';document.getElementById('next').onclick=()=>{slot.innerHTML+='<div role="dialog" aria-modal="true" aria-label="Inner modal" style="position:fixed;inset:50px;background:lightblue;z-index:30"><button id="finish">Finish nested</button></div>';document.getElementById('finish').onclick=()=>{out.textContent='nested finished';slot.replaceChildren();};};};`),
  '/frame-panels': page('Framed panels', '<iframe title="Partner panels" src="/panels" style="width:1000px;height:700px"></iframe>'),
  '/windows': page('Script windows', '<button id=open>Open script window</button><button id=reuse>Reuse script window</button>', "document.querySelector('#open').onclick=()=>window.open('/table','report','width=700,height=500');reuse.onclick=()=>window.open('/scope','report','width=700,height=500');"),
  '/drag': page('Drag surface','<button id=source draggable=true>Drag source</button><div role=button aria-label="Drop target" id=target style="margin:60px;width:150px;height:100px;background:lightblue">Drop here</div><p id=out></p>',"source.ondragstart=e=>e.dataTransfer.setData('text/plain','payload');target.ondragover=e=>e.preventDefault();target.ondrop=e=>{e.preventDefault();out.textContent='dropped '+e.dataTransfer.getData('text/plain');};"),
  '/many-tables': page('Many tables',Array.from({length:3},()=>'<table><tr><th>Name</th></tr><tr><td>value</td></tr></table>').join('')+'<iframe src="/three-tables"></iframe>'),
  '/three-tables': page('More tables',Array.from({length:3},()=>'<table><tr><th>Name</th></tr><tr><td>frame value</td></tr></table>').join('')),
  '/native-dialog': page('Native dialog','<button id=show>Show native dialog</button><dialog id=modal aria-labelledby=title><h2 id=title>Native confirmation</h2><button id=done>Close native dialog</button></dialog><p id=out></p>',"show.onclick=()=>modal.showModal();done.onclick=()=>{modal.close();out.textContent='native done';};"),
  '/shadow-dialog': page('Shadow dialog','<x-dialog></x-dialog><p id=out></p>',"customElements.define('x-dialog',class extends HTMLElement{connectedCallback(){const s=this.attachShadow({mode:'open'});s.innerHTML='<button id=open>Open shadow dialog</button><div id=slot></div>';s.getElementById('open').onclick=()=>{s.getElementById('slot').innerHTML='<div role=dialog aria-label=Shadow style=position:fixed;inset:60px;background:white><button id=close>Close shadow dialog</button></div>';s.getElementById('close').onclick=()=>{s.getElementById('slot').replaceChildren();document.querySelector('#out').textContent='shadow closed';};};}});"),
  '/scope': page('Scopes', `<section role=region aria-label="First form"><label>First name <input></label></section><section role=region aria-label="Second form"><label>Second name <input></label></section>`),

  '/login': page('Sign in', `<header><nav><a href="/">Home</a><a href="/help">Help</a><a href="/pricing">Pricing</a></nav></header>
<main><h1>Sign in</h1><form id=f><label>Email <input name=email type=email></label><br><label>Password <input name=password type=password></label><br>
<label><input type=checkbox name=remember> Remember me</label><br><button type=submit>Sign in</button></form><p id=msg></p></main>
<footer>${Array.from({length:30},(_,i)=>`<a href="/f${i}">Footer link ${i}</a>`).join(' ')}</footer>`,
  `f.onsubmit=async e=>{e.preventDefault();const d=Object.fromEntries(new FormData(f));const r=await fetch('/api/login',{method:'POST',body:JSON.stringify(d)});const j=await r.json();
   if(j.ok){document.querySelector('main').innerHTML='<h1>Dashboard</h1><p>Welcome, '+j.user+'</p><button>Sign out</button>';history.pushState({},'','/dashboard');}else msg.textContent='Invalid credentials';}`),
  '/signup': page('Create account', `<main><h1>Create account</h1><form id=f>
<label>Full name <input name=name></label><br><label>Email <input name=email></label><br><label>Phone <input name=phone></label><br>
<label>Country <select name=country><option value="">Choose…</option><option value=cn>China</option><option value=de>Germany</option><option value=us>United States</option></select></label><br>
<fieldset><legend>Plan</legend><label><input type=radio name=plan value=free> Free</label><label><input type=radio name=plan value=pro> Pro</label><label><input type=radio name=plan value=team> Team</label></fieldset>
<label><input type=checkbox name=terms> I accept the terms</label><br><label><input type=checkbox name=news checked> Send me newsletters</label><br>
<label>About you <textarea name=about></textarea></label><br><button>Create account</button></form><p id=msg role=status></p></main>`,
  `f.onsubmit=async e=>{e.preventDefault();const d=Object.fromEntries(new FormData(f));if(!d.terms){msg.textContent='Please accept the terms';return;}await fetch('/api/signup',{method:'POST',body:JSON.stringify(d)});msg.textContent='Account created for '+d.name;}`),
  '/feed': page('Feed', `<main><h1>Product feed</h1><ul id=list></ul><p id=detail></p></main>`,
  `let n=0;function more(){for(let i=0;i<20;i++){n++;const li=document.createElement('li');const b=document.createElement('button');b.textContent='Item '+n;const k=n;b.onclick=()=>{detail.textContent='Selected item '+k+' (SKU-'+(1000+k)+')';};li.appendChild(b);list.appendChild(li);}}
   more();addEventListener('scroll',()=>{if(innerHeight+scrollY>=document.body.scrollHeight-50&&n<200)setTimeout(more,150);});`),
  '/modal': page('Reports', `<main><h1>Reports</h1><button id=dl>Download report</button><p id=out></p></main>
<div class=overlay id=ov><div class=box role=dialog aria-label="Cookie consent"><p>We use cookies.</p><button id=acc>Accept all</button><button>Settings</button></div></div>`,
  `acc.onclick=()=>ov.remove();dl.onclick=()=>{out.textContent='Report ready: Q3.pdf';fetch('/api/report',{method:'POST',body:'{}'});};`),
  '/spa': page('Shop', `<nav><a href="#/" data-r=home>Home</a><a href="#/products" data-r=products>Products</a><a href="#/contact" data-r=contact>Contact</a></nav><main id=app></main>`,
  `const P={'widget-pro':['Widget Pro','$129'],'widget-mini':['Widget Mini','$49'],'gadget':['Gadget X','$99']};
   function r(){const h=location.hash.slice(1)||'/';if(h==='/')app.innerHTML='<h1>Welcome</h1><p>Browse our products.</p>';else if(h==='/products')app.innerHTML='<h1>Products</h1>'+Object.entries(P).map(([k,v])=>'<a href="#/p/'+k+'">'+v[0]+'</a>').join(' ');
   else if(h.startsWith('/p/')){const v=P[h.slice(3)];setTimeout(()=>{app.innerHTML='<h1>'+v[0]+'</h1><p>Price: '+v[1]+'</p><button>Add to cart</button>';},400);app.innerHTML='<p>Loading…</p>';}else app.innerHTML='<h1>Contact</h1>';}
   addEventListener('hashchange',r);r();`),
  '/slow': page('Analytics', `<main><h1>Analytics</h1><button id=go>Load data</button><div id=res></div></main>`,
  `go.onclick=()=>{res.innerHTML='<p>Loading…</p>';setTimeout(()=>{res.innerHTML='<h2>Results</h2><p>Total orders: 42</p><p>Revenue: $8,410</p>';},2500);};`),
  '/iframe': page('Support', `<main><h1>Support portal</h1><p>Search our knowledge base below.</p><iframe src="/frame" style="width:600px;height:200px"></iframe></main>`),
  '/frame': page('KB', `<label>Search articles <input id=q></label><button id=s>Go</button><p id=r></p>`,
  `s.onclick=()=>{r.textContent=q.value?('3 articles found for "'+q.value+'"'):'Type a query';};`),
  '/table': page('Invoices', `<main><h1>Invoices</h1><table><thead><tr><th>Invoice</th><th>Customer</th><th>Amount</th><th>Status</th></tr></thead><tbody>${
    Array.from({length:30},(_,i)=>`<tr><td>INV-${100+i}</td><td>Customer ${String.fromCharCode(65+i%26)}</td><td>${(i*37%500)+50}</td><td>${i%4===1?'overdue':i%4===2?'pending':'paid'}</td></tr>`).join('')}</tbody></table></main>`),
  '/todo': page('Todos', `<main><h1>Todos</h1><input id=t placeholder="What needs to be done?" aria-label="New todo"><button id=a>Add</button><ul id=l></ul><p id=c></p></main>`,
  `function cnt(){c.textContent=l.querySelectorAll('input:not(:checked)').length+' items left';}a.onclick=()=>{if(!t.value)return;const li=document.createElement('li');const cb=document.createElement('input');cb.type='checkbox';cb.setAttribute('aria-label','Done: '+t.value);cb.onchange=cnt;li.append(cb,' '+t.value);l.appendChild(li);t.value='';cnt();};t.onkeydown=e=>{if(e.key==='Enter')a.click();};`),
  // ---- fault-injection / diverse-page fixtures (faults.test.cjs) ----
  '/dupes': page('Cart', `<main><h1>Cart</h1><ul id=l><li>Apple <button>Remove</button></li><li>Banana <button>Remove</button></li><li>Cherry <button>Remove</button></li></ul><p id=out></p><button id=ins>Add Durian on top</button></main>`,
  `l.onclick=e=>{if(e.target.tagName==='BUTTON'){out.textContent='Removed '+e.target.parentNode.firstChild.textContent.trim();e.target.parentNode.remove();}};
   ins.onclick=()=>{const li=document.createElement('li');li.innerHTML='Durian <button>Remove</button>';l.prepend(li);};`),
  '/samelist': page('Rows', `<main><h1>Rows</h1><div id=g><div class=r><button>Edit</button></div><div class=r><button>Edit</button></div></div><p id=out></p></main>`,
  `let n=0;g.querySelectorAll('button').forEach((b,i)=>b.onclick=()=>out.textContent='edit '+i);window.shuffle=()=>{const [a,b]=g.children;g.insertBefore(b,a);};`),
  '/shadow': page('Shadow', `<main><h1>Settings</h1><x-panel></x-panel><p id=out></p></main>`,
  `customElements.define('x-panel',class extends HTMLElement{connectedCallback(){const r=this.attachShadow({mode:'open'});r.innerHTML='<label>Nickname <input id=n></label><button id=s>Save nickname</button>';r.getElementById('s').onclick=()=>{document.getElementById('out').textContent='Saved '+r.getElementById('n').value;};}});`),
  '/dialogs': page('Dialogs', `<main><h1>Danger zone</h1><button id=d>Delete account</button><button id=a>Show alert</button><p id=out></p></main>`,
  `d.onclick=()=>{out.textContent=confirm('Really delete?')?'Deleted':'Kept';};a.onclick=()=>{alert('Heads up');out.textContent='Alerted';};`),
  '/popup': page('Popup', `<main><h1>Docs</h1><a href="/table" target=_blank>Open invoices in new tab</a></main>`),
  // ---- iframes: nested, cross-site (OOPIF via 127.0.0.2), dynamic ----
  '/nested': page('Nested', `<main><h1>Nested frames</h1><iframe src="/frame-outer" style="width:700px;height:420px"></iframe></main>`),
  '/frame-outer': page('Outer', `<h2>Outer frame</h2><button id=o>Outer action</button><p id=or></p><iframe src="/frame" style="width:600px;height:200px"></iframe>`, `o.onclick=()=>or.textContent='outer clicked';`),
  '/xorigin': page('Partner', `<main><h1>Partner widget</h1><div id=slot></div></main>`, `const f=document.createElement('iframe');f.src='http://127.0.0.2:'+location.port+'/frame';f.style.cssText='width:600px;height:200px';slot.appendChild(f);`),
  '/dynframe': page('Chat', `<main><h1>Help center</h1><button id=oc>Open chat</button><button id=cc>Close chat</button><div id=slot></div></main>`,
  `oc.onclick=()=>{if(!slot.firstChild){const f=document.createElement('iframe');f.src='/frame';f.style.cssText='width:600px;height:200px';slot.appendChild(f);}};cc.onclick=()=>slot.replaceChildren();`),
  // ---- files ----
  '/upload': page('Upload', `<main><h1>Upload</h1><label>Avatar <input type=file id=av></label><br>
<input type=file id=docs multiple hidden><button id=pick>Attach documents</button><br><button id=nope>Not an uploader</button><p id=out role=status></p></main>`,
  `const show=async(t,fs)=>{const parts=[];for(const f of fs)parts.push(f.name+' '+f.size+' bytes "'+(await f.text()).slice(0,20)+'"');out.textContent=t+': '+parts.join('; ');};
   av.onchange=()=>show('avatar',av.files);docs.onchange=()=>show('docs',docs.files);pick.onclick=()=>docs.click();`),
  '/download': page('Exports', `<main><h1>Exports</h1><a href="/files/report.csv">Download CSV</a> <button id=blob>Export JSON</button> <a href="/files/broken">Broken download</a> <a href="/files/slow">Slow download</a> <a href="/table">Open invoices</a></main>`,
  `blob.onclick=()=>{const a=document.createElement('a');a.href=URL.createObjectURL(new Blob([JSON.stringify({ok:true,n:3})],{type:'application/json'}));a.download='export.json';a.click();};`),
  // ---- canvas (no a11y tree: only image coordinates can hit it) ----
  '/canvas': page('Canvas', `<main><h1>Seat map</h1><canvas id=c width=400 height=200 style="display:block;border:1px solid #000"></canvas><p id=out>none</p></main>`,
  `const g=c.getContext('2d');g.fillStyle='#16a34a';g.fillRect(300,100,80,80);c.onclick=e=>{const r=c.getBoundingClientRect(),x=e.clientX-r.left,y=e.clientY-r.top;out.textContent=(x>=300&&x<=380&&y>=100&&y<=180)?'seat booked':'missed '+Math.round(x)+','+Math.round(y);};`),
  '/navaway': page('Nav', `<main><h1>Nav</h1><button id=go>Go slowly</button></main>`, `go.onclick=()=>setTimeout(()=>location.href='/table',300);`),
};
function start(port = 0) {
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://x');
    if (req.method === 'POST') { let b = ''; req.on('data', c => b += c); req.on('end', () => { const d = JSON.parse(b || '{}'); record.push({ path: url.pathname, d });
      res.setHeader('content-type', 'application/json'); res.end(JSON.stringify(url.pathname === '/api/login' ? (d.email === 'alice@example.com' && d.password === 's3cret' ? { ok: true, user: 'Alice' } : { ok: false }) : { ok: true })); }); return; }
    if (url.pathname === '/api/record') { res.setHeader('content-type', 'application/json'); return res.end(JSON.stringify(record)); }
    if (url.pathname === '/files/report.csv') { res.setHeader('content-type', 'text/csv'); res.setHeader('content-disposition', 'attachment; filename="report Q3.csv"'); return res.end('invoice,amount\nINV-100,50\nINV-101,87\n'); }
    if (url.pathname === '/files/broken') { res.setHeader('content-type', 'application/octet-stream'); res.setHeader('content-disposition', 'attachment; filename="broken.bin"'); res.setHeader('content-length', '100000'); res.write(Buffer.alloc(1000)); return setTimeout(() => res.destroy(), 200); }
    if (url.pathname === '/files/slow') { res.setHeader('content-type', 'application/octet-stream'); res.setHeader('content-disposition', 'attachment; filename="slow.bin"'); res.setHeader('content-length', '6000'); let i = 0; const t = setInterval(() => { res.write(Buffer.alloc(1000)); if (++i === 6) { clearInterval(t); res.end(); } }, 1000); req.on('close', () => clearInterval(t)); return; }
    const html = pages[url.pathname]; res.statusCode = html ? 200 : 404; res.setHeader('content-type', 'text/html'); res.end(html || page('Not found', '<h1>Not found</h1>'));
  });
  return new Promise(r => server.listen(port, '0.0.0.0', () => r({ server, port: server.address().port, record, reset: () => record.splice(0) })));
}
module.exports = { start };
if (require.main === module) start(Number(process.argv[2]) || 8099).then(s => console.log('fixtures on', s.port));
