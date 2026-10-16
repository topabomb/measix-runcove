#!/usr/bin/env python3
"""Qualification-only MCP bridge for an isolated model test, not a product tool.
Credentials and initialize.instructions never enter its output or model context.
"""
import sys,json,urllib.request
from browser_client import Client,BASE
client=Client('desktop');client.initialize()
try:
 if sys.argv[1]=='tools':result=client.call('tools/list',{})
 else:
  args=json.load(open(sys.argv[3])) if len(sys.argv)>3 else {}
  result=client.tool(sys.argv[2],args)
  for block in result.get('content',[]):
   if block.get('type')=='image':block.clear();block.update(type='text',text='Image omitted by this text-only qualification bridge; product MCP returns a separate image block.')
 print(json.dumps(result,ensure_ascii=False))
finally:
 request=urllib.request.Request(BASE+'/u/desktop/mcp',method='DELETE',headers={'Authorization':'Bearer '+client.token,'Mcp-Session-Id':client.session,'MCP-Protocol-Version':'2025-11-25'})
 try:
  with urllib.request.urlopen(request,timeout=10) as response:response.read()
 except Exception:pass
