'use strict';
// Real Chromium + the production per-request worker. Run in the pinned Workspace
// image. The HTTP guard below is a fixture; Rust admission is verified separately.
const {test,before,after}=require('node:test'),assert=require('node:assert/strict');
const fs=require('node:fs'),cp=require('node:child_process'),crypto=require('node:crypto'),http=require('node:http');
const {chromium}=require('/opt/agent-tools/node_modules/playwright');
const fixtures=require('/tests/fixtures/structured-browser-site.cjs');
let chrome,browser,context,page,site,guard;const captures=new Map();
const get=async p=>(await fetch('http://127.0.0.1:9222'+p)).json();
const pageIds=new WeakMap();
const id=async p=>{if(pageIds.has(p))return pageIds.get(p);const s=await context.newCDPSession(p);try{const id=(await s.send('Target.getTargetInfo')).targetInfo.targetId;pageIds.set(p,id);return id;}finally{await s.detach();}};
const brief=r=>JSON.stringify(r,(k,v)=>k==='images'?'[images]':v).slice(0,5000);
async function call(operation,args={},page_id=null){
 const result='/tmp/browser-tool-'+crypto.randomUUID()+'.json';
 const spec={operation,args,page_id:page_id||await id(page),guard_port:guard.address().port,task_id:crypto.randomUUID(),captures:[...captures.values()]};
 const r=await new Promise(resolve=>cp.execFile(process.execPath,['/opt/agent-space/desktop/browser-worker.cjs'],{env:{...process.env,AGENT_TASK:JSON.stringify(spec),AGENT_RESULT:result},timeout:45000,maxBuffer:200000},(error,stdout,stderr)=>resolve({error,stdout,stderr})));
 assert.ok(fs.existsSync(result),r.stderr);const body=JSON.parse(fs.readFileSync(result));fs.unlinkSync(result);
 for(const c of body.captures||[])captures.set(c.image_id,c);
 if(r.error&&!body.error)throw Error(r.stderr||String(r.error));return body;
}
const good=async(op,args,pid)=>{const r=await call(op,args,pid);assert.ok(!r.error,brief(r));return r.value;};
const act=async(actions,extra={})=>good('act',{actions,...extra});
const ref=(v,role,name)=>{const l=(v.elements||'').split('\n').find(l=>l.split(' ')[1]===role&&l.includes(JSON.stringify(name)));assert.ok(l,brief(v));return l.split(' ')[0];};
async function fresh(route){for(const extra of context.pages())if(extra!==page)await extra.close();await page.goto('http://127.0.0.1:'+site.port+route);await page.waitForTimeout(150);return good('snapshot');}
before(async()=>{
 site=await fixtures.start();
 chrome=cp.spawn(chromium.executablePath(),[...(process.env.HEADED?[]:['--headless=new']),'--no-sandbox','--no-first-run','--no-default-browser-check','--disable-gpu','--no-proxy-server','--remote-debugging-port=9222','--user-data-dir=/tmp/runcove-browser-node-profile','--window-size=1440,900',...(process.env.DEVICE_SCALE?['--force-device-scale-factor='+process.env.DEVICE_SCALE]:[]),'--host-resolver-rules=MAP *.test 127.0.0.1','about:blank'],{stdio:process.env.CHROME_DEBUG?'inherit':'ignore'});
 for(let i=0;i<100;i++){try{if((await get('/json/version')).webSocketDebuggerUrl)break;}catch{}await new Promise(r=>setTimeout(r,100));}
 browser=await chromium.connectOverCDP('http://127.0.0.1:9222');context=browser.contexts()[0];page=context.pages()[0];
 guard=http.createServer(async(req,res)=>{const p=await context.newPage();res.setHeader('content-type','application/json');res.end(JSON.stringify({page_id:await id(p)}));});await new Promise(r=>guard.listen(0,'127.0.0.1',r));
});
after(async()=>{if(browser)await browser.close().catch(()=>{});if(chrome)chrome.kill('SIGKILL');if(site){site.server.closeAllConnections();site.server.close();}if(guard){guard.closeAllConnections();guard.close();}});

test('representative text payload sizes',async()=>{
 await fresh('/login');const read=await good('read',{query:'Email'});
 const v=await good('snapshot');const action=await act([{do:'fill',ref:ref(v,'textbox','Email'),text:'example@example.com'}]);
 await fresh('/panels');await page.evaluate(()=>{const shell=document.createElement('div');shell.style.cssText='position:fixed;right:0;top:0';shell.innerHTML='<nav><button>Site menu</button></nav>';document.body.append(shell);});
 const panel=await good('snapshot');await act([{do:'click',ref:ref(panel,'button','Actions')}]);const menu=await good('snapshot');
 console.log('METRICS='+JSON.stringify(Object.fromEntries(Object.entries({read,action,menu}).map(([k,v])=>[k,{bytes:Buffer.byteLength(JSON.stringify(v)),value:v}]))));
});
