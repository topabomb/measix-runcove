'use strict';
// Real Chromium + the production per-request worker. Run in the pinned Workspace
// image. The HTTP guard below is a fixture; Rust admission is verified separately.
const {test,before,after}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),cp=require('node:child_process'),crypto=require('node:crypto'),http=require('node:http');
const {chromium}=require('/opt/agent-tools/node_modules/playwright');
const fixtures=require('./fixtures/structured-browser-site.cjs');
let chrome,browser,context,page,site,guard;const captures=new Map();
const get=async p=>(await fetch('http://127.0.0.1:9222'+p)).json();
const pageIds=new WeakMap();
const id=async p=>{if(pageIds.has(p))return pageIds.get(p);const s=await context.newCDPSession(p);try{const id=(await s.send('Target.getTargetInfo')).targetInfo.targetId;pageIds.set(p,id);return id;}finally{await s.detach();}};
const brief=r=>JSON.stringify(r,(k,v)=>k==='images'?'[images]':v).slice(0,5000);
const baselines={}; let observationOrder=0;
async function call(operation,args={},page_id=null){
 args={...args,_baselines:baselines,_observation_order:++observationOrder};
 const result='/tmp/browser-tool-'+crypto.randomUUID()+'.json';
 const spec={operation,args,page_id:page_id||await id(page),guard_port:guard.address().port,task_id:crypto.randomUUID(),captures:[...captures.values()]};
 const r=await new Promise(resolve=>cp.execFile(process.execPath,['/opt/agent-space/desktop/browser-worker.cjs'],{env:{...process.env,AGENT_TASK:JSON.stringify(spec),AGENT_RESULT:result},timeout:90000,maxBuffer:200000},(error,stdout,stderr)=>resolve({error,stdout,stderr})));
 assert.ok(fs.existsSync(result),r.stderr);const body=JSON.parse(fs.readFileSync(result));fs.unlinkSync(result);
 if(body.value?._observation){const b=body.value._observation;baselines[b.page_id]=b;delete body.value._observation;}
 for(const c of body.captures||[])captures.set(c.image_id,c);
 if(r.error&&!body.error)throw Error(r.stderr||String(r.error));return body;
}
const good=async(op,args,pid)=>{const r=await call(op,args,pid);assert.ok(!r.error,brief(r));return r.value;};
const act=async(actions,extra={})=>good('act',{actions,...extra});
const ref=(v,role,name)=>{const l=(v.observation||'').split('\n').map(l=>l.trim()).find(l=>l.split(' ')[1]===role&&l.includes(JSON.stringify(name)));assert.ok(l,brief(v));return l.split(' ')[0];};
async function fresh(route){for(const extra of context.pages())if(extra!==page)await extra.close();await page.goto('http://127.0.0.1:'+site.port+route);await page.waitForTimeout(150);return good('snapshot');}
before(async()=>{
 site=await fixtures.start();
 chrome=cp.spawn(chromium.executablePath(),[...(process.env.HEADED?[]:['--headless=new']),'--no-sandbox','--no-first-run','--no-default-browser-check','--disable-gpu','--no-proxy-server','--remote-debugging-port=9222','--user-data-dir=/tmp/runcove-browser-node-profile','--window-size=1440,900',...(process.env.DEVICE_SCALE?['--force-device-scale-factor='+process.env.DEVICE_SCALE]:[]),'--host-resolver-rules=MAP *.test 127.0.0.1','about:blank'],{stdio:process.env.CHROME_DEBUG?'inherit':'ignore'});
 for(let i=0;i<100;i++){try{if((await get('/json/version')).webSocketDebuggerUrl)break;}catch{}await new Promise(r=>setTimeout(r,100));}
 browser=await chromium.connectOverCDP('http://127.0.0.1:9222');context=browser.contexts()[0];page=context.pages()[0];
 guard=http.createServer(async(req,res)=>{const p=await context.newPage();res.setHeader('content-type','application/json');res.end(JSON.stringify({page_id:await id(p)}));});await new Promise(r=>guard.listen(0,'127.0.0.1',r));
});
after(async()=>{if(browser)await browser.close().catch(()=>{});if(chrome)chrome.kill('SIGKILL');if(site){site.server.closeAllConnections();site.server.close();}if(guard){guard.closeAllConnections();guard.close();}});

test('field refs are immediately usable without an earlier snapshot',async()=>{
 await page.goto('http://127.0.0.1:'+site.port+'/login');
 const r=await good('read',{extract:'fields'});const email=r.fields.find(f=>f.name==='Email');assert.ok(email,brief(r));
 await act([{do:'fill',ref:email.ref,text:'alice@example.com'}]);assert.equal(await page.locator('input[name=email]').inputValue(),'alice@example.com');
});
test('unique replaced nodes and new documents never inherit old refs',async()=>{
 let v=await fresh('/login'),r=ref(v,'button','Sign in');
 await page.locator('button[type=submit]').evaluate(e=>e.outerHTML=e.outerHTML);
 let failed=await call('act',{actions:[{do:'click',ref:r}]});assert.equal(failed.error?.code,'STALE_REF',brief(failed));assert.equal(failed.side_effects,'not_started');
 v=await good('snapshot');r=ref(v,'button','Sign in');await page.reload();
 failed=await call('act',{actions:[{do:'click',ref:r}]});assert.equal(failed.error?.code,'STALE_REF',brief(failed));
});

test('batch feedback has scoped text once, old states, hidden removals and snapshot fallback',async()=>{
 await fresh('/login');
 await page.evaluate(()=>document.body.innerHTML='<section aria-label="Upload"><button id="toggle" aria-expanded="false">Add files</button><div id="menu" hidden role="menu"><button>Choose file</button><p role="status">Ready</p></div><button id="cancel">Cancel upload</button></section><section aria-label="History"><p>Ready</p></section>');
 await page.evaluate(()=>document.querySelector('#toggle').onclick=()=>{document.querySelector('#toggle').setAttribute('aria-expanded','true');document.querySelector('#menu').hidden=false;document.querySelector('#cancel').hidden=true;});
 const initial=await good('snapshot');
 const result=await act([{do:'click',ref:ref(initial,'button','Add files')}]);
 assert.equal(result.completed,1);assert.equal(result.of,1);
 assert.ok(!result.layers && !result.diff && !result.text,brief(result));
 assert.match(result.observation_diff,/- .*button "Add files"/);
 assert.match(result.observation_diff,/\+ .*button "Add files" \[expanded\]/);
 assert.match(result.observation_diff,/- .*button "Cancel upload"/);
 assert.equal(await page.locator('#cancel').count(),1,'observation removal is not a DOM deletion');
 assert.equal((result.observation_diff.match(/Choose file/g)||[]).length,1,brief(result));
 assert.match(result.observation_diff,/menu[\s\S]*status "Ready"/);
 assert.equal((await act([{do:'wait',text:'Ready'}])).observation_diff,'');
 const full=await good('snapshot');assert.ok(full.observation);assert.equal(full.observation_diff,undefined);
 await page.locator('#menu [role=status]').evaluate(e=>e.textContent='Selected');
 const changed=await act([{do:'wait',text:'Selected'}]);
 assert.match(changed.observation_diff,/status "Selected"/);
 assert.equal((await act([{do:'wait',text:'Selected'}])).observation_diff,'');
 // Navigate explicitly: document replacement must never become a massive removal patch.
 const navigated=await act([{do:'navigate',url:'data:text/html,<button>New document</button>'}]);
 assert.ok(navigated.observation);assert.equal(navigated.observation_diff,undefined);
});
test('duplicate node identity survives insertion and reordering',async()=>{
 const v=await fresh('/dupes'),remove=(v.observation||'').split('\n').filter(l=>l.includes('button "Remove"')).map(l=>l.trim().split(' ')[0]);assert.equal(remove.length,3,brief(v));
 await act([{do:'click',ref:ref(v,'button','Add Durian on top')}]);await act([{do:'click',ref:remove[1]}]);assert.equal(await page.locator('#out').innerText(),'Removed Banana');
});
for(const route of ['/iframe','/nested','/xorigin'])test('frame content is observable and actionable: '+route,async()=>{
 const v=await fresh(route);const q=ref(v,'textbox','Search articles'),go=ref(v,'button','Go');
 const matches=(await good('read',{query:'Search articles'})).matches;assert.ok(matches.some(m=>m.ref===q&&m.iframe),brief(matches));
 await act([{do:'fill',ref:q,text:'widgets'},{do:'click',ref:go},{do:'wait',text:'3 articles found'}]);
 const read=await good('read');assert.match(read.text,/3 articles found/);const field=await good('read',{ref:q});assert.ok(field);
 if(route==='/xorigin')assert.ok((await get('/json/list')).some(t=>t.type==='iframe'),'must exercise a real OOPIF');
});
test('removed/reinserted iframe invalidates its old refs',async()=>{
 let v=await fresh('/dynframe');await act([{do:'click',ref:ref(v,'button','Open chat')},{do:'wait',text:'Search articles'}]);
 v=await good('snapshot');const q=ref(v,'textbox','Search articles');await act([{do:'click',ref:ref(v,'button','Close chat')}]);
 let r=await call('act',{actions:[{do:'fill',ref:q,text:'bad'}]});assert.equal(r.error?.code,'STALE_REF',brief(r));
 v=await good('snapshot');await act([{do:'click',ref:ref(v,'button','Open chat')},{do:'wait',text:'Search articles'}]);
 r=await call('act',{actions:[{do:'fill',ref:q,text:'bad'}]});assert.equal(r.error?.code,'STALE_REF',brief(r));
 v=await good('snapshot');assert.notEqual(ref(v,'textbox','Search articles'),q);
});
test('popup reports its page identity and is readable and closable',async()=>{
 const v=await fresh('/popup');const r=await act([{do:'click',ref:ref(v,'link','Open invoices in new tab')}]);assert.equal(r.popups?.length,1,brief(r));
 const pid=r.popups[0].page_id;assert.ok(pid);const table=await good('read',{extract:'table'},pid);assert.equal(table.tables[0].rows.length,30,brief(table));
 await act([{do:'tab',action:'close',page_id:pid}]);assert.equal(context.pages().length,1);
});
test('JS menu, hover tooltip, popover and nested modal are observable and interactive',async()=>{
 let v=await fresh('/panels');await act([{do:'click',ref:ref(v,'button','Actions')}]);v=await good('snapshot');assert.match(v.observation,/menu/);
 await act([{do:'click',ref:ref(v,'menuitem','Choose item')}]);assert.equal(await page.locator('#out').innerText(),'item chosen');
 v=await good('snapshot');await act([{do:'hover',ref:ref(v,'button','Hover for help')}]);v=await good('snapshot');assert.match(v.observation,/tooltip[\s\S]*Helpful/);
 await page.mouse.move(0,0);v=await good('snapshot');await act([{do:'click',ref:ref(v,'button','Open popover')}]);v=await good('snapshot');assert.match(v.observation,/Popover content/);
 await act([{do:'click',ref:ref(v,'button','Confirm popover')}]);v=await good('snapshot');await act([{do:'click',ref:ref(v,'button','Open nested modal')}]);
 v=await good('snapshot');await act([{do:'click',ref:ref(v,'button','Open inner modal')}]);v=await good('snapshot');assert.equal((v.observation.match(/dialog /g)||[]).length,2,brief(v));
 await act([{do:'click',ref:ref(v,'button','Finish nested')}]);assert.equal(await page.locator('#out').innerText(),'nested finished');
});
test('floating panels inside an iframe expose usable frame refs',async()=>{
 let v=await fresh('/frame-panels');await act([{do:'click',ref:ref(v,'button','Actions')}]);v=await good('snapshot');assert.match(v.observation,/iframe[\s\S]*menu[\s\S]*Choose item/);await act([{do:'click',ref:ref(v,'menuitem','Choose item')}]);assert.match((await good('read')).text,/item chosen/);
});
test('script new windows and named-window reuse preserve page identity',async()=>{
 let v=await fresh('/windows');const parent=await id(page);let r=await act([{do:'click',ref:ref(v,'button','Open script window')}]);assert.equal(r.popups?.length,1,brief(r));const child=r.popups[0].page_id;assert.ok(child);assert.equal((await good('read',{extract:'table'},child)).tables[0].rows.length,30);
 v=await good('snapshot',{},parent);r=await act([{do:'click',ref:ref(v,'button','Reuse script window')}]);assert.equal(context.pages().length,2);assert.equal((await good('read',{extract:'fields'},child)).fields.length,2);await act([{do:'tab',action:'close',page_id:child}]);
});
test('signup workflow, shadow DOM and navigation history use structured steps',async()=>{
 let v=await fresh('/signup');await act([{do:'fill',ref:ref(v,'textbox','Full name'),text:'Alice'},{do:'select',ref:ref(v,'combobox','Country'),value:'China'},{do:'check',ref:ref(v,'checkbox','I accept the terms')},{do:'click',ref:ref(v,'button','Create account')},{do:'wait',text:'Account created for Alice'}]);const records=(await(await fetch('http://127.0.0.1:'+site.port+'/api/record')).json());assert.equal(records.at(-1).d.country,'cn');
 v=await fresh('/shadow');await act([{do:'fill',ref:ref(v,'textbox','Nickname'),text:'Shadow'},{do:'click',ref:ref(v,'button','Save nickname')}]);assert.match((await good('read')).text,/Saved Shadow/);
 await act([{do:'navigate',url:'http://127.0.0.1:'+site.port+'/table'},{do:'navigate',mode:'back'}]);assert.match((await good('read')).text,/Settings/);await act([{do:'navigate',mode:'forward'},{do:'navigate',mode:'reload'}]);assert.equal((await good('read',{extract:'table'})).tables[0].rows.length,30);
});
test('native HTML dialogs and shadow-root dialogs are identified and interactive',async()=>{
 for(const [route,open,close,label] of [['/native-dialog','Show native dialog','Close native dialog','Native confirmation'],['/shadow-dialog','Open shadow dialog','Close shadow dialog','Shadow']]){
  let v=await fresh(route);await act([{do:'click',ref:ref(v,'button',open)}]);v=await good('snapshot');assert.ok(v.observation.includes('dialog '+JSON.stringify(label)),brief(v));assert.ok(ref(v,'button',close));if(route==='/native-dialog')assert.match(v.observation,/dialog.*\[modal\]/);await act([{do:'click',ref:ref(v,'button',close)}]);
 }
});
test('scoped field extraction excludes other forms and issues working refs',async()=>{
 const v=await fresh('/scope');const first=ref(v,'region','First form');const r=await good('read',{ref:first,extract:'fields'});assert.equal(r.fields.length,1,brief(r));assert.equal(r.fields[0].name,'First name');
 await act([{do:'fill',ref:r.fields[0].ref,text:'done'}]);assert.equal(await page.locator('input').first().inputValue(),'done');
});
test('screen page capture covers the entire document without scrolling and bottom coordinates work',async()=>{
 await fresh('/long');if(process.env.DEVICE_SCALE==='2')await page.locator('#top + div').evaluate(e=>e.style.height='1800px');await page.evaluate(()=>scrollTo(0,400));await page.waitForTimeout(100);
 const before=await page.evaluate(()=>({x:scrollX,y:scrollY,w:innerWidth,h:innerHeight,events:{...events}}));
 const r=cp.execFileSync(process.execPath,['/opt/agent-space/desktop/observe-worker.cjs',JSON.stringify({view:'page',page_id:await id(page)})],{encoding:'utf8',timeout:10000,maxBuffer:20*1024*1024});
 const shot=JSON.parse(r),bytes=Buffer.from(shot.images[0].data,'base64'),c=shot.capture;captures.set(c.image_id,c);
 assert.ok(c.full_page);assert.ok(c.content_height>(process.env.DEVICE_SCALE==='2'?1800:6000));assert.equal(bytes.readUInt32BE(20),c.height);assert.equal(c.height,Math.round(c.content_height*c.scale));assert.ok(c.height>before.h*c.scale);
 const after=await page.evaluate(()=>({x:scrollX,y:scrollY,w:innerWidth,h:innerHeight,events:{...events}}));assert.deepEqual({...after,events:undefined},{...before,events:undefined});
 const end=await page.locator('#end').evaluate(e=>{const r=e.getBoundingClientRect();return {x:r.right-20,y:r.y+scrollY+r.height/2};});
 const pixel=await page.evaluate(async([data,x,y])=>{const i=new Image();i.src='data:image/png;base64,'+data;await i.decode();const c=document.createElement('canvas');c.width=i.width;c.height=i.height;const g=c.getContext('2d');g.drawImage(i,0,0);return [...g.getImageData(Math.floor(x),Math.floor(y),1,1).data];},[shot.images[0].data,end.x*c.scale,end.y*c.scale]);assert.ok(pixel[0]>240&&pixel[1]<20&&pixel[2]<20,'bottom rendered pixels must be in the image');
 const point=await page.locator('#bottom').evaluate(e=>{const r=e.getBoundingClientRect();return {x:r.x+scrollX+r.width/2,y:r.y+scrollY+r.height/2};});
 await act([{do:'click',image_id:c.image_id,x:point.x*c.scale,y:point.y*c.scale}]);assert.equal(await page.locator('#out').innerText(),'bottom clicked');
});
test('page images retain correct pixel mapping at native device scale 2',async t=>{
 if(process.env.DEVICE_SCALE!=='2'){t.skip('Run again with DEVICE_SCALE=2 for native device scaling');return;}
 await fresh('/canvas');assert.equal(await page.evaluate(()=>devicePixelRatio),2);
 const r=await call('snapshot',{image:true});assert.ok(!r.error,brief(r));const dimensions=await page.evaluate(async data=>{const i=new Image();i.src='data:image/png;base64,'+data;await i.decode();return {w:i.width,h:i.height};},r.images[0].data);
 assert.equal(dimensions.w,r.value.image.width);assert.equal(dimensions.h,r.value.image.height);assert.equal(await page.evaluate(()=>devicePixelRatio),2);
 const box=await page.locator('canvas').boundingBox(),image=r.value.image;await act([{do:'click',image_id:image.image_id,x:(box.x+340)*image.scale,y:(box.y+140)*image.scale}]);assert.equal(await page.locator('#out').innerText(),'seat booked');
 await fresh('/long');const oversized=await call('snapshot',{image:true});assert.match(JSON.stringify(oversized.error),/IMAGE_TOO_LARGE/);assert.ok(!oversized.images);await page.locator('#top + div').evaluate(e=>e.style.height='1800px');const long=await good('snapshot',{image:true});assert.ok(long.image.height>2000,brief(long));assert.equal(long.image.scale,2);
});
test('ref and image drags deliver browser drag events',async()=>{
 let v=await fresh('/drag');await act([{do:'drag',ref:ref(v,'button','Drag source'),to_ref:ref(v,'button','Drop target')}]);assert.equal(await page.locator('#out').innerText(),'dropped payload');
 await fresh('/drag');v=await good('snapshot',{image:true});const from=await page.locator('#source').boundingBox(),to=await page.locator('#target').boundingBox(),scale=v.image.scale;
 await act([{do:'drag',image_id:v.image.image_id,x:(from.x+from.width/2)*scale,y:(from.y+from.height/2)*scale,to_x:(to.x+to.width/2)*scale,to_y:(to.y+to.height/2)*scale}]);assert.equal(await page.locator('#out').innerText(),'dropped payload');
});
test('table bounds apply globally across iframes',async()=>{
 await fresh('/many-tables');const r=await good('read',{extract:'table'});assert.equal(r.tables.length,5);assert.equal(r.truncated,true);assert.ok(r.tables.some(t=>JSON.stringify(t).includes('frame value')));
});
test('passive tools do not dismiss an already-open JS dialog',async()=>{
 await fresh('/dialogs');let held;const hold=d=>held=d;page.on('dialog',hold);
 const showing=page.evaluate(()=>{document.querySelector('#out').textContent=confirm('Pending user decision')?'accepted pending':'dismissed pending';}).catch(()=>{});
 try{
  await new Promise(r=>setTimeout(r,200));const v=await good('snapshot');assert.equal(v.health.page,false,brief(v));assert.equal(v.browser_ui,'page_blocked');
  const read=await call('read');assert.equal(read.error?.code,'PAGE_UNRESPONSIVE',brief(read));assert.equal(held.message(),'Pending user decision');
 }finally{await held?.dismiss().catch(()=>{});page.off('dialog',hold);await showing;}
});
test('Unicode limits preserve full scalar characters and report text removals',async()=>{
 await fresh('/login');await page.evaluate(()=>{document.title='😀'.repeat(201);document.body.innerHTML='<main><p id=text></p></main>';document.querySelector('#text').textContent='😀'.repeat(5000);});
 const read=await good('read',{max_chars:1});assert.equal(read.text,'😀');assert.equal(read.chars,5000);assert.equal(read.truncated,true);
 await good('snapshot');await page.locator('#text').evaluate(e=>e.remove());const full=await good('snapshot');assert.ok(full.observation !== undefined,brief(full));
 await page.evaluate(()=>document.body.innerHTML='<textarea aria-label="Long field">'+ '😀'.repeat(3000)+'</textarea>');const fields=await good('read',{extract:'fields'});assert.equal(Array.from(fields.fields[0].value).length,1000);assert.equal(fields.truncated,true);
 const value=await good('read',{ref:fields.fields[0].ref,max_chars:4000});assert.equal(Array.from(value.text).length,3000);
 const multiline='中文\nhello "quoted" \\ 🙂';await page.locator('textarea').fill(multiline);assert.equal((await good('read',{extract:'fields'})).fields[0].value,multiline);
 const label='Path "quoted" \\ 中文';await page.locator('textarea').evaluate((e,label)=>e.setAttribute('aria-label',label),label);assert.equal((await good('read',{extract:'fields'})).fields[0].name,label);
 await page.evaluate(()=>document.body.innerHTML='<select aria-label="Country"><option value="cn" selected>China</option></select>');assert.equal((await good('read',{extract:'fields'})).fields[0].value,'cn');
 await page.evaluate(()=>document.body.innerHTML='<main>'+ '😀'.repeat(5000)+'</main>');const inline=await good('read',{max_chars:5000});assert.equal(inline.text,'😀'.repeat(5000));
});
test('whole-batch validation rejects later invalid steps before any click',async()=>{
 const v=await fresh('/panels');const r=await call('act',{actions:[{do:'click',ref:ref(v,'button','Actions')},{do:'click',ref:'invalid'}]});assert.equal(r.error?.code,'INVALID_ARGUMENT',brief(r));assert.equal(await page.locator('[role=menu]').count(),0);
});
test('failed scroll and already-clicked single chooser report possible side effects',async()=>{
 await fresh('/feed');let r=await call('act',{actions:[{do:'scroll',until_text:'never exists',max:1}]});assert.equal(r.error?.code,'NOT_FOUND',brief(r));assert.equal(r.side_effects,'may_have_occurred');assert.ok(await page.evaluate(()=>scrollY)>0);
 const v=await fresh('/upload');await page.evaluate(()=>docs.multiple=false);fs.mkdirSync('/workspace/browser-tests',{recursive:true});fs.writeFileSync('/workspace/browser-tests/a.txt','alpha');fs.writeFileSync('/workspace/browser-tests/b.txt','beta');
 r=await call('act',{actions:[{do:'upload',ref:ref(v,'button','Attach documents'),paths:['/workspace/browser-tests/a.txt','/workspace/browser-tests/b.txt']}]});assert.equal(r.error?.code,'INVALID_ARGUMENT',brief(r));assert.equal(r.side_effects,'may_have_occurred');
});
test('upload and download preserve exact bytes through production outputs',async()=>{
 fs.mkdirSync('/workspace/browser-tests',{recursive:true});fs.writeFileSync('/workspace/browser-tests/a.txt','alpha');
 let v=await fresh('/upload');await act([{do:'upload',ref:ref(v,'button','Attach documents'),paths:['/workspace/browser-tests/a.txt']}]);assert.match(await page.locator('#out').innerText(),/alpha/);
 v=await fresh('/download');const r=await call('act',{actions:[{do:'click',ref:ref(v,'link','Download CSV')}]});assert.ok(!r.error,brief(r));assert.equal(r.files.length,1,brief(r));assert.match(fs.readFileSync(r.files[0].path,'utf8'),/INV-100,50/);
});

test('wait matches visible text and accessible names literally across frames and shadow DOM',async()=>{
 await fresh('/login');
 await page.setContent('<input placeholder="Prompt input"><button aria-label="Upload image"></button><img alt="Generated [1].png"><p aria-hidden="true">Visible only</p><p hidden>Hidden result</p><div id="shadow"></div><iframe srcdoc="<button aria-label=FrameName></button>"></iframe>');
 await page.locator('#shadow').evaluate(e=>e.attachShadow({mode:'open'}).innerHTML='<button aria-label="ShadowName"></button>');
 await act(['prompt INPUT','upload IMAGE','generated [1].png','visible ONLY','framename','shadowname'].map(text=>({do:'wait',text})));
 let failed=await call('act',{actions:[{do:'wait',text:'Hidden result'}]});assert.equal(failed.error?.code,'TIMEOUT',brief(failed));
 failed=await call('act',{actions:[{do:'wait',gone:'upload image'}]});assert.equal(failed.error?.code,'TIMEOUT',brief(failed));
 await page.getByRole('button',{name:'Upload image',exact:true}).evaluate(e=>e.remove());await act([{do:'wait',gone:'upload image'}]);
 // Regex metacharacters must stay literal, not match any label containing a dot.
 failed=await call('act',{actions:[{do:'wait',text:'Generated .png'}]});assert.equal(failed.error?.code,'TIMEOUT',brief(failed));
});
test('query ref scopes matches and ordinary results omit redundant healthy metadata',async()=>{
 const v=await fresh('/scope');assert.equal(v.tabs,undefined);const tabs=await good('snapshot',{view:'tabs'});assert.ok(tabs.tabs?.length);assert.equal(tabs.observation,undefined);
 const r=await good('read',{ref:ref(v,'region','First form'),query:'name'});
 assert.equal(r.matches.length,1,brief(r));assert.equal(r.matches[0].name,'First name');assert.equal(r.tabs,undefined);if(!v.health)assert.equal(r.health,undefined);else assert.equal(r.health.desktop,false);
 const changed=await act([{do:'fill',ref:r.matches[0].ref,text:''}]);assert.equal(changed.tabs,undefined);if(!v.health)assert.equal(changed.health,undefined);else assert.equal(changed.health.desktop,false);
 const focused=await act([{do:'tab',action:'focus'}]);assert.equal(focused.tabs,undefined);
});
test('stale second step preserves completed count, side effects and failure metadata',async()=>{
 const v=await fresh('/login'),button=ref(v,'button','Sign in');
 await page.locator('button[type=submit]').evaluate(e=>e.outerHTML=e.outerHTML);
 const r=await call('act',{actions:[{do:'fill',ref:ref(v,'textbox','Email'),text:'once@example.com'},{do:'click',ref:button}]});
 assert.equal(r.error?.code,'STALE_REF',brief(r));assert.equal(r.value.completed,1,brief(r));assert.equal(r.failed_step.step_number,2,brief(r));assert.equal(r.side_effects,'may_have_occurred');assert.ok(r.value.page_id);assert.equal(await page.locator('input[name=email]').inputValue(),'once@example.com');
});

test('rounded preview dimensions map each coordinate axis independently',async()=>{
 await fresh('/login');await page.setContent('<style>body{margin:0;width:1440px;height:10000px}button{position:absolute;left:100px;top:9950px;width:40px;height:2px;border:0;padding:0}</style><button aria-label="Thin target" onclick="window.hit=true"></button>');
 const v=await good('snapshot',{image:true}),c=captures.get(v.image.image_id);
 // Match the shared Rust resize's integer dimensions, deliberately nonuniform
 // by a fraction of a pixel: 1440x10000 -> 369x2560.
 c.width=369;c.height=2560;c.scale=c.width/c.content_width;
 await act([{do:'click',image_id:c.image_id,x:120*c.width/c.content_width,y:9951*c.height/c.content_height}]);
 assert.equal(await page.evaluate(()=>window.hit),true);
});

test('native process exit codes never become numeric tool error codes',()=>{
 const {classify}=require('/opt/agent-space/desktop/browser-tools.cjs');
 assert.equal(classify({code:1,message:'Command failed: observe\nThe operation was aborted due to timeout'}).code,'TIMEOUT');
 assert.equal(classify({code:2,message:'Command failed'}).code,'ACTION_FAILED');
 assert.equal(classify({code:'STALE_REF',message:'diagnostic',detail:'observe again'}).message,'observe again');
 assert.equal(classify({code:'ENOENT',message:'missing file'}).code,'ENOENT');
});

test('seconds waits honor real timers, early conditions and the 60 second default',async()=>{
 await page.setContent('<button onclick="window.marked=performance.now()">Mark</button>');
 let v=await good('snapshot'); const mark=ref(v,'button','Mark');
 const start=await page.evaluate(()=>performance.now());
 await act([{do:'wait',secs:2},{do:'click',ref:mark}],{then:'diff'});
 const elapsed=await page.evaluate(start=>window.marked-start,start);
 assert.ok(elapsed>=2000,`fixed wait ended early: ${elapsed}`);
 await page.evaluate(()=>setTimeout(()=>document.body.append('ready after old default'),11000));
 const began=Date.now(); await act([{do:'wait',text:'ready after old default'}],{then:'diff'});
 const conditionMs=Date.now()-began;assert.ok(conditionMs>=10000&&conditionMs<30000,`condition wait: ${conditionMs}`);
 const timeoutStart=Date.now();const failed=await call('act',{actions:[{do:'wait',text:'never present'}],then:'diff'});
 assert.equal(failed.error?.code,'TIMEOUT',brief(failed));assert.ok(Date.now()-timeoutStart>=59000);
 console.log('WAIT_TIMING '+JSON.stringify({fixedSeconds:2,measuredMs:elapsed,defaultConditionMeasuredMs:conditionMs,conditionTimeoutSeconds:60}));
});

test('fixed waits do not access a page or request observations',async()=>{
 const {run}=require('/opt/agent-space/desktop/browser-tools.cjs');
 const unexpected=()=>{throw Error('delay touched browser');};
 const start=Date.now(); const result=await run({operation:'act',page_id:'missing',args:{actions:[{do:'wait',secs:1}],then:'snapshot'}},{checkpoint:()=>{},page:unexpected,observe:unexpected});
 assert.equal(result.value.completed,1); assert.equal(result.value.of,1);assert.ok(Date.now()-start>=1000);
 assert.equal(result.value.observation,undefined);
});

test('successful action survives a failed final observation without advancing its baseline',async()=>{
 const v=await fresh('/login');
 const {run}=require('/opt/agent-space/desktop/browser-tools.cjs');
 const result=await run({operation:'act',page_id:await id(page),args:{actions:[{do:'fill',ref:ref(v,'textbox','Email'),text:'saved@example.com'}]}},{
  page:async()=>page,checkpoint:()=>{},output:{},
  observe:async()=>{throw Error('PAGE_UNRESPONSIVE: injected final observation failure');}
 });
 assert.equal(result.value.completed,1);assert.equal(result.error,undefined);
 assert.equal(result.value.observation_error.code,'PAGE_UNRESPONSIVE');
 assert.equal(result.value._observation,undefined);
 assert.equal(await page.locator('input[name=email]').inputValue(),'saved@example.com');
});
test('merged actions use refs, coordinates and navigation without retired names',async()=>{
 let v=await fresh('/login');const email=ref(v,'textbox','Email');
 await act([{do:'fill',ref:email,text:'preserved'},{do:'press',key:'Control+a',ref:email},{do:'press',key:'Backspace',ref:email}]);
 assert.equal(await page.locator('input[name=email]').inputValue(),'');
 await act([{do:'click',ref:email,image_id:{unused:true},x:'unused',y:-1},{do:'scroll',ref:email,dy:90000,until_text:{},max:false},{do:'navigate',mode:'reload',url:false}]);
 const r=await call('act',{actions:[{do:'fill',ref:email,text:'must not run'},{do:'click_at',image_id:'retired',x:0,y:0}]});
 assert.equal(r.error.code,'INVALID_ARGUMENT');assert.equal(await page.locator('input[name=email]').inputValue(),'');
});

test('long document URLs and titles stay bounded and explicitly truncated',async()=>{
 await page.goto('data:text/html,'+encodeURIComponent('<title>'+ '界'.repeat(300) +'</title><p>readable</p>'));
 for (const operation of ['snapshot','read']) {
  const result=await good(operation);
  assert.ok(Array.from(result.url).length<=1001);assert.equal(result.truncated,true);
  if(result.title)assert.ok(Array.from(result.title).length<=201);
  assert.ok(operation==='snapshot'||result.text.includes('readable'));
 }
  const screen=JSON.parse(cp.execFileSync(process.execPath,['/opt/agent-space/desktop/observe-worker.cjs',JSON.stringify({view:'page',page_id:await id(page)})],{encoding:'utf8',maxBuffer:16*1024*1024}));
  assert.equal(screen.truncated,true);assert.equal(Array.from(screen.title).length,200);
  await fresh('/login');
  const popupUrl='http://127.0.0.1:'+site.port+'/login?long='+'x'.repeat(1500);
  await page.setContent('<a target="_blank" href="'+popupUrl+'">Long popup</a>');
  const opened=await act([{do:'click',ref:ref(await good('snapshot'),'link','Long popup')}]);
  assert.equal(opened.popups[0].truncated,true);assert.equal(Array.from(opened.popups[0].url).length,1000);
 await page.setContent('<a download="bounded.txt" href="data:text/plain,'+'x'.repeat(3000)+'">Download</a>');
 const v=await good('snapshot');const result=await call('act',{actions:[{do:'download',ref:ref(v,'link','Download')}]});
 assert.ok(!result.error,brief(result));assert.equal(result.value.downloads[0].truncated,true);
 assert.ok(Array.from(result.value.downloads[0].url).length<=1001);
 assert.equal(fs.readFileSync(result.files[0].path,'utf8'),'x'.repeat(3000));
});

test('query context distinguishes duplicate controls and respects subtree scope',async()=>{
 await fresh('/login');
 await page.evaluate(()=>document.body.innerHTML='<section aria-label="Project A"><button>Save</button></section><section aria-label="Project B"><button>Save</button></section>');
 const snapshot=await good('snapshot');
 const found=await good('read',{query:'Save'});
 const buttons=found.matches.filter(m=>m.role==='button');assert.equal(buttons.length,2,brief(found));
 assert.deepEqual(buttons.map(m=>m.context),['region "Project A"','region "Project B"']);
 const scoped=await good('read',{query:'Save',ref:ref(snapshot,'region','Project B')});
 assert.equal(scoped.matches.filter(m=>m.role==='button').length,1);
 assert.equal(scoped.matches.find(m=>m.role==='button').ref,buttons[1].ref);
 const empty=await good('read',{query:'absent-target'});assert.deepEqual(empty.matches,[]);
 await page.locator('section').first().evaluate(e=>e.remove());
 const afterRead=await act([{do:'wait',gone:'absent-target'}]);
 assert.match(afterRead.observation_diff,/- .*region "Project A"/,'read does not replace the act baseline');
});
