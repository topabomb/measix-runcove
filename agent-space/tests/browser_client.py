"""Live qualification support. Requires a dedicated test Host and credential file.
Never stores or prints credentials; initialize instructions are deliberately ignored.
"""
import json,urllib.request,os
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
def text(result):
 raw=next(c["text"] for c in result["content"] if c["type"]=="text")
 try:return json.loads(raw)
 except json.JSONDecodeError:return {'text':raw}
