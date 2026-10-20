"""Live qualification support. Requires a dedicated test Host and credential file.
Never stores or prints credentials; initialize instructions are deliberately ignored.
"""
import json,urllib.request,os,re,shlex
from pathlib import Path
Q=Path(os.environ["AGENT_SPACE_BROWSER_TEST_DIR"])
BASE=os.environ.get("AGENT_SPACE_BROWSER_TEST_URL","http://127.0.0.1:"+(Q/"port").read_text())
USERS=json.loads((Q/"users.json").read_text())
TARGET=(Q/"target").read_text().strip() if (Q/"target").exists() else "server"
def space_path(name,suffix):
 return ("" if TARGET=="standalone" else "/u/"+name)+"/"+suffix.lstrip("/")
class Client:
 def __init__(self,name):self.name=name;self.token=USERS[name]["token"];self.session=None;self.counter=0
 def call(self,method,params=None):
  self.counter+=1;headers={"Authorization":"Bearer "+self.token,"Content-Type":"application/json","Accept":"application/json, text/event-stream","MCP-Protocol-Version":"2025-11-25"}
  if self.session:headers["Mcp-Session-Id"]=self.session
  data={"jsonrpc":"2.0","id":self.counter,"method":method}
  if params is not None:data["params"]=params
  request=urllib.request.Request(BASE+space_path(self.name,"mcp"),data=json.dumps(data).encode(),headers=headers)
  with urllib.request.urlopen(request,timeout=90) as response:
   if response.headers.get("Mcp-Session-Id"):self.session=response.headers["Mcp-Session-Id"]
   raw=response.read().decode()
  if raw.startswith("event:") or raw.startswith("id:") or raw.startswith("data:"):raw=next(line[5:].strip() for line in raw.splitlines() if line.startswith("data:") and '"id"' in line)
  result=json.loads(raw)
  if "error" in result:raise RuntimeError(result["error"])
  return result["result"]
 def initialize(self):return self.call("initialize",{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"qualification","version":"1"}})
 def tool(self,name,args):return self.call("tools/call",{"name":name,"arguments":args})
 def finish(self,reply):
  value=text(reply)
  while value.get('state')=='running':
   reply=self.tool('browser_act',{'op':'wait','task_id':value['task_id']});value=text(reply)
  return value,reply
 def act(self,actions,**args):return self.finish(self.tool('browser_act',{'actions':actions,**args}))
 def good(self,name,args=None):
  value,reply=self.finish(self.tool(name,args or {}))
  assert not reply.get('isError'),value
  return value.get('value',value)
 def snapshot(self,**args):
  value,reply=self.finish(self.tool('browser_snapshot',args));assert not reply.get('isError'),value
  result=value.get('value',value)
  return {**result,**{k:value[k] for k in ['control','recent_tasks'] if k in value}}
 def read_browser(self,**args):return self.good('browser_read',args)
 def navigate(self,url,**args):
  value,reply=self.act([{'do':'navigate','url':url}],then='snapshot',**args)
  assert not reply.get('isError'),value
  return value['value']
 def fixture_js(self,code,page_id=None):
  """Browser fixture setup/readback; requires a desktop workspace, never a product tool."""
  script="const {chromium}=require('/opt/agent-tools/node_modules/playwright');(async()=>{const browser=await chromium.connectOverCDP('http://127.0.0.1:9222',{noDefaults:true,timeout:5000});const context=browser.contexts()[0];let page=context.pages()[0];"
  if page_id:
   script+="for(const c of browser.contexts())for(const p of c.pages()){const s=await c.newCDPSession(p);const info=await s.send('Target.getTargetInfo');await s.detach();if(info.targetInfo.targetId==="+json.dumps(page_id)+")page=p;}"
  script+="const value=await(async()=>{"+code+"})();console.log('QUALIFICATION_JSON='+JSON.stringify(value??null));process.exit();})().catch(e=>{console.error(e);process.exit(1)});"
  result=self.good('bash',{'cmd':'node -e '+shlex.quote(script)})
  raw=result.get('text','')
  match=re.search(r'QUALIFICATION_JSON=(.*)',raw)
  assert match,result
  return json.loads(match[1])

def element(value,role,name):
 lines=value.get('elements','').splitlines()
 match=next((line for line in lines if line.split(' ',2)[1:2]==[role] and json.dumps(name,ensure_ascii=False) in line),None)
 assert match,value
 return match.split(' ',1)[0]

def reset_tabs(client,url='http://127.0.0.1:8124/'):
 snapshot=client.snapshot(view='tabs');tabs=snapshot['tabs']
 if not tabs:
  value,reply=client.act([{'do':'tab','action':'new','url':url}]);assert not reply.get('isError'),value
 else:
  keep=tabs[0]['page_id'];steps=[{'do':'tab','action':'close','page_id':tab['page_id']} for tab in tabs[1:]]
  steps+=[{'do':'tab','action':'focus','page_id':keep},{'do':'navigate','url':url}]
  value,reply=client.act(steps,page_id=keep);assert not reply.get('isError'),value
 return client.snapshot()
def text(result):
 raw=next(c["text"] for c in result["content"] if c["type"]=="text")
 try:return json.loads(raw)
 except json.JSONDecodeError:return {'text':raw}
