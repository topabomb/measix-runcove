const fs=require('node:fs'),cp=require('node:child_process'),crypto=require('node:crypto');
const {chromium}=require('/opt/agent-tools/node_modules/playwright');
const {compile}=require('/opt/agent-space/desktop/script.cjs');
const {publish,directory}=require('/opt/agent-space/desktop/artifacts.cjs');
const {desktopCapture}=require('/opt/agent-space/desktop/observation.cjs');
const task=JSON.parse(process.env.AGENT_TASK);delete process.env.AGENT_TASK;
const captures=new Map((task.captures||[]).map(c=>[c.image_id,c]));
const images=[],files=[],emittedCaptures=[];let bytes=0;
// Atomic checkpoints retain explicitly registered output when later code fails
// or the execution owner terminates the worker.
function checkpoint(extra={}){
  const result={...extra};
  if(images.length)result.images=images;if(files.length)result.files=files;
  if(emittedCaptures.length)result.captures=emittedCaptures;
  const path=process.env.AGENT_RESULT;
  fs.writeFileSync(path+'.tmp',JSON.stringify(result));fs.renameSync(path+'.tmp',path);
}
const cmd=(name,args)=>cp.execFileSync(name,args,{encoding:'utf8',timeout:4000}).trim();
const front=()=>cmd('xdotool',['getactivewindow']);
function screenshot(){
  const c=desktopCapture();captures.set(c.image_id,c);if(captures.size>32)captures.delete(captures.keys().next().value);return c;
}
function active(){if(task.window && front()!==task.window)throw Error('STALE_OBSERVATION: foreground window changed; observe desktop again');}
function coordinate(c){
  if(!c||typeof c!=='object'||typeof c.image_id!=='string'||!Number.isFinite(c.x)||!Number.isFinite(c.y))throw Error('INVALID_ARGUMENT: use desktop.click({image_id,x,y}), desktop.drag({image_id,x,y,to_x,to_y}), or desktop.scroll({image_id,x,y,delta})');
  const s=captures.get(c.image_id);if(!s||!s.window||Date.now()-s.at>60000||s.window!==front())throw Error('STALE_OBSERVATION: capture expired or foreground window changed; observe desktop again');
  if(c.x<0||c.y<0||c.x>=s.width||c.y>=s.height)throw Error('INVALID_ARGUMENT: coordinates must be inside the returned screenshot');
  const width=s.original_width||s.width,height=s.original_height||s.height;
  return [String(Math.min(width-1,Math.round(c.x*width/s.width))),String(Math.min(height-1,Math.round(c.y*height/s.height)))];
}
const desktop={
  screenshot:async()=>screenshot(),
  click:async c=>{const xy=coordinate(c);cmd('xdotool',['mousemove',...xy,'click',String(c.button||1)]);},
  drag:async c=>{const xy=coordinate(c),end=coordinate({...c,x:c.to_x,y:c.to_y});cmd('xdotool',['mousemove',...xy,'mousedown','1','mousemove',...end,'mouseup','1']);},
  scroll:async c=>{const xy=coordinate(c),delta=c.delta;if(!Number.isInteger(delta)||Math.abs(delta)>30)throw Error('INVALID_ARGUMENT: scroll delta must be -30..30');if(delta)cmd('xdotool',['mousemove',...xy,'click','--repeat',String(Math.abs(delta)),delta>0?'5':'4']);},
  type:async text=>{
    active();if(typeof text!=='string'||text.length>65536)throw Error('invalid text');
    const clip=cp.spawn('xclip',['-selection','clipboard','-quiet'],{stdio:['pipe','ignore','ignore']});clip.stdin.end(text);
    try{await new Promise(r=>setTimeout(r,100));cmd('xdotool',['key','--clearmodifiers','ctrl+v']);await new Promise(r=>setTimeout(r,200));}
    finally{clip.kill('SIGTERM');}
  },
  press:async key=>{active();if(typeof key!=='string'||!/^[A-Za-z0-9_+ -]{1,80}$/.test(key))throw Error('invalid key');cmd('xdotool',['key','--clearmodifiers',key.replaceAll('Control','ctrl').replaceAll('Enter','Return')]);},
};
const output={file:async file=>{if(files.length>=16)throw Error('At most 16 artifact files per task');const item=publish(file);files.push(item);checkpoint();return item;},download:async download=>{const name=download.suggestedFilename().replace(/[^\p{L}\p{N}._-]/gu,'_');const temp=directory+'/'+crypto.randomUUID()+'-'+name;await download.saveAs(temp);return await output.file(temp);},image:async capture=>{
  const b=Buffer.isBuffer(capture)?capture: capture?.bytes;
  if(!Buffer.isBuffer(b))throw Error('INVALID_ARGUMENT: output.image requires a PNG, JPEG, or WebP Buffer, or a desktop Capture');
  const mime=b.subarray(0,8).equals(Buffer.from([137,80,78,71,13,10,26,10]))?'image/png':b[0]===255&&b[1]===216?'image/jpeg':b.toString('ascii',0,4)==='RIFF'&&b.toString('ascii',8,12)==='WEBP'?'image/webp':null;
  if(!mime)throw Error('INVALID_ARGUMENT: output.image supports PNG, JPEG, and WebP');
  if(images.length>=4||bytes+b.length>8*1024*1024)throw Error('image output exceeds 4 images / 8 MiB');bytes+=b.length;
  images.push({type:'image',mimeType:mime,data:b.toString('base64')});
  if(capture.image_id){const {bytes,...metadata}=capture;emittedCaptures.push(metadata);}checkpoint();
},saveMhtml:async(page,path)=>{if(!path.startsWith('/workspace/'))throw Error('MHTML destination must be in /workspace');const s=await page.context().newCDPSession(page);try{const {data}=await s.send('Page.captureSnapshot',{format:'mhtml'});fs.writeFileSync(path,data);return await output.file(path);}finally{await s.detach();}}};
(async()=>{
  // A fresh Playwright attachment can consume/cancel an already-open native chooser.
  // Native foreground observations deliberately do not attach; the same tool runs desktop helpers.
  const desktopOnly=task.nativeWindow||task.pageResponsive===false;
  const browser=desktopOnly?null:await chromium.connectOverCDP('http://127.0.0.1:9222',{timeout:5000});
  const unavailable=new Proxy({}, {get(){throw Error(task.nativeWindow?'NATIVE_WINDOW: use desktop helpers, then observe after the dialog closes':'PAGE_UNRESPONSIVE: use desktop helpers to recover the tab, then observe');}});
  const context=browser?browser.contexts()[0]:unavailable;
  if(browser){context.setDefaultTimeout(Math.min(task.timeoutMs,10000));context.setDefaultNavigationTimeout(Math.min(task.timeoutMs,20000));}
  let page=desktopOnly?unavailable:null;
  const matches=await Promise.all((browser?context.pages():[]).map(async candidate=>{
    let s;try{return await Promise.race([(async()=>{s=await context.newCDPSession(candidate);const {targetInfo}=await s.send('Target.getTargetInfo');return targetInfo.targetId===task.page_id?candidate:null;})(),new Promise(r=>setTimeout(()=>r(null),2000))]);}catch{return null;}finally{if(s)await Promise.race([s.detach().catch(()=>{}),new Promise(r=>setTimeout(r,200))]);}
  }));
  if(browser)page=matches.find(Boolean);
  if(!page)throw Error('STALE_OBSERVATION: page no longer exists');
  desktop.setWindowMode=async mode=>{
    if(!browser)throw Error('NATIVE_WINDOW: close native dialog before changing browser window mode');
    if(!['normal','maximized','fullscreen'].includes(mode))throw Error('invalid window mode');
    const s=await browser.newBrowserCDPSession();try{
      const {windowId,bounds}=await s.send('Browser.getWindowForTarget',{targetId:task.page_id});
      const until=Date.now()+4000;
      const set=async windowState=>{
        await s.send('Browser.setWindowBounds',{windowId,bounds:{windowState}});
        while(Date.now()<until){
          const actual=await s.send('Browser.getWindowBounds',{windowId});
          if(actual.bounds.windowState===windowState)return actual;
          await new Promise(r=>setTimeout(r,50));
        }
        throw Error('WINDOW_MODE_TIMEOUT: observe the desktop before retrying');
      };
      if(bounds.windowState!=='normal')await set('normal');
      return await set(mode);
    }finally{await s.detach();}
  };
  // Cooperation guard, not a hostile-JavaScript sandbox.
  if(browser){context.close=async()=>{throw Error('managed context cannot be closed');};
  browser.close=async()=>{throw Error('managed browser cannot be closed');};
  browser.newContext=async()=>{throw Error('only the shared context is supported');};}
  const value=await compile(task.code)(page,context,desktop,output,directory);
  let serialized,outputWarning=null;
  try{serialized=JSON.stringify(value===undefined?null:value)??'null';}catch(e){serialized='null';outputWarning='Execution completed, return value is not JSON serializable: '+e.message;}
  if(Buffer.byteLength(serialized)>32768){
    const temp=directory+'/'+crypto.randomUUID()+'.json';fs.writeFileSync(temp,serialized);
    const item=await output.file(temp);serialized=JSON.stringify({output_file:item.path,bytes:Buffer.byteLength(serialized)});outputWarning='Execution completed; large return value saved as JSON artifact.';
  }
  // Restore ordinary user downloads after a Playwright download operation.
  if(browser){const s=await browser.newBrowserCDPSession();await s.send('Browser.setDownloadBehavior',{behavior:'default'}).catch(()=>{});await s.detach();}
  checkpoint({value:JSON.parse(serialized),...(outputWarning?{output_warning:outputWarning}:{})});
  process.stdout.write('',()=>process.exit(0));
})().catch(error=>{
  let message=error instanceof Error ? (error.name+': '+error.message) : String(error);
  if(message.includes('\nCall log:')){
    const target=message.split('\nCall log:')[1].split('\n').find(line=>line.includes('waiting for'));
    message=message.split('\nCall log:')[0]+(target?'\n'+target.trim().slice(0,400):'');
  }
  console.error(message==='ReferenceError: require is not defined' ? 'require is not available in this script. Import modules with await import(...).' : message);process.exit(1);
});
