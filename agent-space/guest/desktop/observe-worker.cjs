// Direct CDP observation. Never creates a Playwright attachment, so native
// file choosers remain visible and usable.
'use strict';
const cp=require('node:child_process'),crypto=require('node:crypto');
const {domExpression,compactAX,desktopCapture}=require('./observation.cjs');
const params=JSON.parse(process.argv[2]||'{}');
const cmd=(name,args)=>cp.execFileSync(name,args,{encoding:'utf8',timeout:1500,maxBuffer:65536}).trim();
const get=async path=>{const response=await fetch('http://127.0.0.1:9222'+path,{signal:AbortSignal.timeout(1500)});if(!response.ok)throw Error('CDP_HTTP_'+response.status);return response.json();};
async function cdp(url,method,params={}){
 return new Promise((resolve,reject)=>{
  const ws=new WebSocket(url),timer=setTimeout(()=>finish(Error('CDP_TIMEOUT')),1500);
  function finish(error,value){clearTimeout(timer);ws.close();error?reject(error):resolve(value);}
  ws.onopen=()=>ws.send(JSON.stringify({id:1,method,params}));
  ws.onmessage=event=>{let m;try{m=JSON.parse(event.data);}catch{return;}if(m.id===1)finish(m.error?Error(m.error.message):null,m.result);};
  ws.onerror=()=>finish(Error('CDP_DISCONNECTED'));
 });
}
async function observe(){
 if(params.view==='desktop'){
  const {bytes,...capture}=desktopCapture();
  return {capture,images:[{type:'image',mimeType:'image/png',data:bytes.toString('base64')}]};
 }
 let window=null,nativeWindow=false;
 try{window=cmd('xdotool',['getactivewindow']);nativeWindow=/window id # 0x[1-9a-f]/i.test(cmd('xprop',['-id',window,'WM_TRANSIENT_FOR']));}catch{}
 const version=await get('/json/version');
 if(params.action){await cdp(version.webSocketDebuggerUrl,params.action==='close'?'Browser.close':'Browser.setDownloadBehavior',params.action==='close'?{}:{behavior:'default'}).catch(()=>{});return {ok:true};}
 let targets=(await get('/json/list')).filter(t=>t.type==='page');
 if(params.create_page&&!targets.length&&!nativeWindow){await cdp(version.webSocketDebuggerUrl,'Target.createTarget',{url:'about:blank'});targets=(await get('/json/list')).filter(t=>t.type==='page');}
 const requested=params.page_id;
 let selected=requested?targets.find(t=>t.id===requested):null;
 const tabs=()=>targets.slice(0,50).map(t=>({page_id:t.id,url:t.url.slice(0,1000),title:t.title.slice(0,200)}));
 if(requested&&!selected)return {error:{code:'PAGE_NOT_FOUND',message:'No open tab matches page_id. Select a tab from tabs, or omit page_id.'},tabs:tabs()};
 if(!requested&&!selected&&!nativeWindow){const visible=await Promise.all(targets.slice(0,12).map(async t=>{try{const r=await cdp(t.webSocketDebuggerUrl,'Runtime.evaluate',{expression:'document.hasFocus()',returnByValue:true});return r.result?.value?t:null;}catch{return null;}}));selected=visible.find(Boolean);}
 if(!requested)selected=selected||targets[0];
 if(params.maximize&&selected&&!nativeWindow){const {windowId,bounds}=await cdp(version.webSocketDebuggerUrl,'Browser.getWindowForTarget',{targetId:selected.id});if(bounds.windowState!=='normal')await cdp(version.webSocketDebuggerUrl,'Browser.setWindowBounds',{windowId,bounds:{windowState:'normal'}});await cdp(version.webSocketDebuggerUrl,'Browser.setWindowBounds',{windowId,bounds:{windowState:'maximized'}});}
 let dom=null,ax=null,responsive=false;
 if(selected&&!nativeWindow&&params.view!=='page'){try{
   if(params.view==='ax'){const r=await cdp(selected.webSocketDebuggerUrl,'Accessibility.getFullAXTree');ax=compactAX(r.nodes||[]);responsive=true;}
   else {const value=await cdp(selected.webSocketDebuggerUrl,'Runtime.evaluate',{expression:params.view==='dom'?domExpression:'true',returnByValue:true});responsive=!!value.result?.value;if(params.view==='dom')dom=value.result?.value;}
 }catch{}}
 if(params.view==='page'){
  if(!selected) return {error:{code:'PAGE_NOT_FOUND',message:'No browser page is available'},tabs:tabs()};
  const at=Date.now();
  const r=await cdp(selected.webSocketDebuggerUrl,'Page.captureScreenshot',{format:'png',captureBeyondViewport:false});if(Buffer.byteLength(r.data,'base64')>10*1024*1024)throw Error('IMAGE_TOO_LARGE');
  const bytes=Buffer.from(r.data,'base64'),capture={image_id:crypto.randomUUID(),width:bytes.readUInt32BE(16),height:bytes.readUInt32BE(20),at,source:'page'};
  return {page_id:selected.id,url:selected.url.slice(0,1000),title:selected.title.slice(0,200),capture,images:[{type:'image',mimeType:'image/png',data:r.data}]};
 }
 return {page_id:selected?.id||null,tabs:tabs(),
  health:{browser:true,page:responsive,desktop:!!window,native_window:nativeWindow},
  ...(dom?{dom}:{}),...(ax?{ax}:{}),window,nativeWindow};
}
observe().then(value=>{process.stdout.write(JSON.stringify(value));}).catch(error=>{console.error(error.message);process.exitCode=1;});
