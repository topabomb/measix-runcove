import {test,expect} from '@playwright/test';
import fs from 'node:fs';
import http from 'node:http';
import https from 'node:https';
import net from 'node:net';
import {execFileSync} from 'node:child_process';
const directory=process.env.AGENT_SPACE_BROWSER_TEST_DIR;
if(!directory)throw Error('Use an independent qualification Host; AGENT_SPACE_BROWSER_TEST_DIR is required');
const credentials=JSON.parse(fs.readFileSync(directory+'/users.json','utf8'));
const base=process.env.AGENT_SPACE_BROWSER_TEST_URL||'http://127.0.0.1:'+fs.readFileSync(directory+'/port','utf8').trim();
const standalone=fs.existsSync(directory+'/target')&&fs.readFileSync(directory+'/target','utf8').trim()==='standalone';
const prefix=standalone?'':'/u/desktop';
const mcp=base+prefix+'/mcp';
const url=base+prefix+'/desktop/';
let session:string|undefined,counter=0,initialized=false;
async function tool(name:string,args:unknown){
 const payload=JSON.stringify({jsonrpc:'2.0',id:++counter,method:initialized?'tools/call':'initialize',params:initialized?{name,arguments:args}:{protocolVersion:'2025-11-25',capabilities:{},clientInfo:{name:'ui-qualification',version:'1'}}});
 const response=await new Promise<{raw:string,headers:http.IncomingHttpHeaders,status:number}>((resolve,reject)=>{
  const request=http.request(mcp,{method:'POST',headers:{...(process.env.AGENT_SPACE_BROWSER_TEST_HOST?{Host:process.env.AGENT_SPACE_BROWSER_TEST_HOST}:{}),Authorization:'Bearer '+credentials.desktop.token,'Content-Type':'application/json','Content-Length':Buffer.byteLength(payload),Accept:'application/json, text/event-stream','MCP-Protocol-Version':'2025-11-25',...(session?{'Mcp-Session-Id':session}:{})}},response=>{
   let raw='';response.setEncoding('utf8');response.on('data',chunk=>raw+=chunk);response.on('end',()=>resolve({raw,headers:response.headers,status:response.statusCode!}));
  });request.on('error',reject);request.end(payload);
 });
 const next=response.headers['mcp-session-id'];if(typeof next==='string')session=next;
 let raw=response.raw;if(raw.startsWith('event:')||raw.startsWith('data:'))raw=raw.split('\n').find(line=>line.startsWith('data:')&&line.includes('"id"'))!.slice(5).trim();
 if(!raw)throw Error('Empty MCP reply: status='+response.status+' type='+response.headers['content-type']);
 const envelope=JSON.parse(raw);if(envelope.error)throw Error(JSON.stringify(envelope.error));
 const result=envelope.result;
 if(result?.content){const raw=result.content.find((item:any)=>item.type==='text').text;try{return JSON.parse(raw);}catch{return {text:raw,isError:!!result.isError};}}
 initialized=true;return tool(name,args);
}
async function login(page:any,target=url){
 await page.goto(target);await page.getByRole('textbox',{name:'访问 Token',exact:true}).fill(credentials.desktop.token);
 await page.getByRole('button',{name:'连接桌面',exact:true}).click();await expect(page.getByText('AI 控制 · 仅观看',{exact:true})).toBeVisible({timeout:45000});
 await expect(page.locator('canvas')).toBeVisible();
}
test.describe.configure({mode:'serial'});
test('real VNC keyboard, clipboard, English UI and a second viewer',async({browser})=>{
 test.setTimeout(90000);
 const first=await browser.newContext({locale:'zh-CN',viewport:{width:1440,height:1000}}),second=await browser.newContext({locale:'zh-CN',viewport:{width:1440,height:1000}});
 const page=await first.newPage(),viewer=await second.newPage();
 const errors:string[]=[];page.on('pageerror',e=>errors.push(e.message));

 await login(page);await login(viewer);
 let prepared=await tool('browser_run',{code:"await desktop.setWindowMode('fullscreen');await page.goto('http://127.0.0.1:8124/');await page.waitForFunction(()=>innerHeight===900);return await page.locator('#clipboard').boundingBox();"});
 while(prepared.state==='running')prepared=await tool('browser_task',{task_id:prepared.task_id});
 expect(prepared.state).toBe('succeeded');
 await page.getByRole('button',{name:'接管操作',exact:true}).click();await expect(page.getByText('你正在操作',{exact:true})).toBeVisible();
 await expect(viewer.getByText('他人在操作 · 仅观看',{exact:true})).toBeVisible();
 const remote=prepared.value,canvas=await page.locator('canvas').boundingBox();
 if(!canvas||!remote)throw Error('Missing screen/fixture bounds');
 await expect(page.getByRole('button',{name:'交还 AI',exact:true})).toBeEnabled();
 await page.mouse.click(canvas.x+(remote.x+remote.width/2)*canvas.width/1440,canvas.y+(remote.y+remote.height/2)*canvas.height/900);
 // Allow the remote compositor to apply the delivered pointer focus change.
 await page.waitForTimeout(200);
 await page.keyboard.type('vnc keyboard123',{delay:30});
 const clipboardValue=async()=> (await tool('browser_observe',{})).dom.controls.find((control:any)=>control.name==='Clipboard fixture')?.value;
 await expect.poll(clipboardValue).toBe('vnc keyboard123');
 await page.getByRole('button',{name:'剪贴板',exact:true}).click();
 await page.getByRole('textbox',{name:'本机 → 远程桌面',exact:true}).fill('中文 hello 🙂');
 await page.getByRole('button',{name:'发送到远程剪贴板',exact:true}).click();await expect(page.locator('#clipboardStatus')).toContainText('已发送');
 await page.keyboard.press('Control+v');
 await expect.poll(clipboardValue,{timeout:10000}).toBe('vnc keyboard123中文 hello 🙂');
 await page.keyboard.press('Control+a');await page.keyboard.press('Control+c');
 await expect(page.locator('#clipboardReceive')).toHaveValue('vnc keyboard123中文 hello 🙂');
 await page.getByRole('button',{name:'交还 AI',exact:true}).click();await expect(page.getByText('AI 控制 · 仅观看',{exact:true})).toBeVisible();
 await page.getByRole('combobox',{name:'语言'}).selectOption('en');await expect(page.getByText('AI control · View only',{exact:true})).toBeVisible();
 await page.getByRole('button',{name:'MCP connection',exact:true}).click();await expect(page.getByRole('textbox',{name:'Service address',exact:true})).toHaveValue(mcp);
 await page.getByRole('textbox',{name:'Service address',exact:true}).fill('example.test:9443/demo/u/desktop/mcp');await page.getByRole('button',{name:'Regenerate configuration',exact:true}).click();
 await expect(page.locator('#mcpJson')).toHaveValue(/http:\/\/example.test:9443\/demo\/u\/desktop\/mcp/);
 await page.setViewportSize({width:360,height:740});
 await expect(page.getByRole('button',{name:'Close sidebar',exact:true})).toBeVisible();
 expect(await page.evaluate(()=>document.documentElement.scrollWidth)).toBeLessThanOrEqual(360);
 await page.screenshot({path:directory+'/workbench-en-360.png'});
 await page.getByRole('button',{name:'Close sidebar',exact:true}).click();await page.setViewportSize({width:1440,height:1000});
 await page.screenshot({path:directory+'/workbench-en.png'});
 for(const language of ['en','zh-CN']){
  await page.getByRole('combobox').selectOption(language);
  for(const width of [320,390,1440]){
   await page.setViewportSize({width,height:width===1440?1000:740});
   expect(await page.evaluate(()=>document.documentElement.scrollWidth)).toBeLessThanOrEqual(width);
   await page.screenshot({path:directory+`/workbench-${language}-${width}.png`});
  }
 }
 expect(errors).toEqual([]);
 await first.close();await second.close();
});

test('a connected passive VNC viewer sleeps after 300 seconds and restores its tabs',async({browser})=>{
 test.setTimeout(360000);
 let prepared=await tool('browser_run',{code:"for(const p of context.pages())if(p!==page)await p.close();await page.goto('http://127.0.0.1:8124/');await page.evaluate(()=>localStorage.setItem('vnc-idle-marker','persistent'));return context.pages().map(p=>p.url());"});
 while(prepared.state==='running')prepared=await tool('browser_task',{task_id:prepared.task_id});
 expect(prepared.state).toBe('succeeded');
 const context=await browser.newContext({locale:'zh-CN',viewport:{width:1440,height:1000}}),page=await context.newPage();
 await login(page);
 const started=Date.now();
 // Keep the real RFB connection and UI polling open, without accepted input.
 await expect(page.locator('#overlayTitle')).toHaveText('桌面已休眠',{timeout:330000});
 expect(Date.now()-started).toBeGreaterThan(285000);
 await page.screenshot({path:directory+'/workbench-idle.png'});
 await page.getByRole('button',{name:'唤醒桌面',exact:true}).click();
 await expect(page.getByText('AI 控制 · 仅观看',{exact:true})).toBeVisible({timeout:45000});
 let restored=await tool('browser_run',{code:"return {tabs:context.pages().map(p=>p.url()),marker:await page.evaluate(()=>localStorage.getItem('vnc-idle-marker'))};"});
 while(restored.state==='running')restored=await tool('browser_task',{task_id:restored.task_id});
 if(JSON.stringify(restored.value?.tabs)!==JSON.stringify(prepared.value)){
  console.log('Wake result at readiness:',JSON.stringify(restored));
  await page.waitForTimeout(10000);
  let later=await tool('browser_run',{code:"return {tabs:context.pages().map(p=>p.url()),marker:await page.evaluate(()=>localStorage.getItem('vnc-idle-marker'))};"});
  while(later.state==='running')later=await tool('browser_task',{task_id:later.task_id});
  console.log('Wake result ten seconds later:',JSON.stringify(later));
 }
 expect(restored.state).toBe('succeeded');expect(restored.value.tabs).toEqual(prepared.value);expect(restored.value.marker).toBe('persistent');
 await context.close();
});

test('human takeover allows basic tools and workspace uploads',async({browser})=>{
 test.setTimeout(90000);
 const context=await browser.newContext({locale:'zh-CN'}),page=await context.newPage();
 await login(page);
 await page.getByRole('button',{name:'接管操作',exact:true}).click();
 await expect(page.getByText('你正在操作',{exact:true})).toBeVisible();
 const path='/workspace/human-file-'+Date.now()+'.txt';
 expect(JSON.stringify(await tool('write',{path,content:'old-value'}))).toContain('Wrote');
 expect(JSON.stringify(await tool('edit',{path,old_text:'old-value',new_text:'new-value'}))).toContain('Replaced');
 expect(JSON.stringify(await tool('read',{path}))).toContain('new-value');
 expect(JSON.stringify(await tool('bash',{command:'echo basic-tools-remain-available'}))).toContain('basic-tools-remain-available');
 expect(JSON.stringify(await tool('browser_run',{code:'return 7;'}))).toContain('HUMAN_CONTROL');
 await page.locator('#filesToggle').click();
 await expect(page.locator('#upload')).toBeEnabled();
 await page.locator('#file').setInputFiles({name:'human-upload-'+Date.now()+'.txt',mimeType:'text/plain',buffer:Buffer.from('human upload succeeds')});
 await expect(page.locator('#uploadStatus')).toContainText('已上传',{timeout:30000});
 await expect(page.getByText('你正在操作',{exact:true})).toBeVisible();
 await page.getByRole('button',{name:'交还 AI',exact:true}).click();
 await context.close();
});

test('HTTPS proxy with rewritten Host preserves nested workbench and origin boundary',async({browser})=>{
 test.setTimeout(90000);
 const key=directory+'/proxy-key.pem',cert=directory+'/proxy-cert.pem';
 execFileSync('openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',key,'-out',cert,'-days','1','-subj','/CN=localhost'],{stdio:'ignore'});
 const upstream=new URL(base),mount='/nested/runcove',peers=new Set<net.Socket>();
 const rewrittenHost='upstream.internal:'+upstream.port;
 const proxy=https.createServer({key:fs.readFileSync(key),cert:fs.readFileSync(cert)},(request,response)=>{
  if(!request.url?.startsWith(mount+'/')){response.writeHead(404).end();return;}
  const forward=http.request({hostname:upstream.hostname,port:upstream.port,path:request.url.slice(mount.length),method:request.method,headers:{...request.headers,host:rewrittenHost}},reply=>{
   response.writeHead(reply.statusCode!,reply.headers);reply.pipe(response);
  });forward.on('error',()=>response.writeHead(502).end());request.pipe(forward);
 });
 proxy.on('upgrade',(request,socket,head)=>{
  if(!request.url?.startsWith(mount+'/')){socket.destroy();return;}
  const peer=net.connect(Number(upstream.port),upstream.hostname,()=>{
   const headers=Object.entries({...request.headers,host:rewrittenHost}).map(([name,value])=>name+': '+value).join('\r\n');
   peer.write(`${request.method} ${request.url!.slice(mount.length)} HTTP/1.1\r\n${headers}\r\n\r\n`);
   if(head.length)peer.write(head);peer.pipe(socket);socket.pipe(peer);
  });peers.add(peer);peer.on('close',()=>{peers.delete(peer);socket.destroy();});peer.on('error',()=>socket.destroy());socket.on('error',()=>peer.destroy());socket.on('close',()=>peer.destroy());
 });
 await new Promise<void>(resolve=>proxy.listen(0,'127.0.0.1',resolve));
 const address=proxy.address();if(!address||typeof address==='string')throw Error('proxy address');
 const origin='https://127.0.0.1:'+address.port,path=mount+prefix+'/desktop/';
 const context=await browser.newContext({locale:'zh-CN',ignoreHTTPSErrors:true}),page=await context.newPage();
 try{
  const logged=page.waitForResponse(response=>response.url()===origin+path+'api/login');
  await login(page,origin+path);const nonce=(await (await logged).json()).nonce;
  await expect(page.locator('body')).toHaveClass(/live/);
  const cookie=(await context.cookies()).find(item=>item.name.includes('desktop'));
  expect(cookie?.secure).toBe(true);expect(cookie?.path).toBe(path);
  await page.locator('#mcpToggle').click();await expect(page.locator('#mcpAddress')).toHaveValue(origin+mount+prefix+'/mcp');
  const valid=await context.request.get(origin+path+'api/control',{headers:{Origin:origin,'X-Desktop-Nonce':nonce}});expect(valid.status()).toBe(200);
  const cross=await context.request.get(origin+path+'api/control',{headers:{Origin:'https://wrong.example','X-Desktop-Nonce':nonce}});expect(cross.status()).toBe(403);
  const forged=await context.request.post(origin+path+'api/control',{headers:{Origin:origin,'X-Desktop-Nonce':'wrong'},data:{mode:'AI'}});expect(forged.status()).toBe(403);
  await page.reload();await expect(page.locator('body')).toHaveClass(/live/);
 }finally{
  await context.close();for(const peer of peers)peer.destroy();proxy.closeAllConnections();
  await new Promise<void>(resolve=>proxy.close(()=>resolve()));fs.rmSync(key,{force:true});fs.rmSync(cert,{force:true});
 }
});
