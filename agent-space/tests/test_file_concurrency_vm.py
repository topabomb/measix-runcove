#!/usr/bin/env python3
"""Concurrent MCP requests against an independent Docker/real-VM qualification Host."""
import concurrent.futures
import json
import shlex
import time
import unittest
from browser_client import Client, text

ROOT = "/workspace/file-concurrency"


def call(name, args):
    client = Client("desktop")
    client.initialize()
    result = client.tool(name, args)
    return result


def good(name, args):
    result = call(name, args)
    if result.get("isError"):
        raise AssertionError(text(result))
    return result


class FileConcurrencyVM(unittest.TestCase):
    def test_01_real_guest_has_four_file_processes_and_queues_the_rest(self):
        good("bash", {"cmd": "mkdir -p " + ROOT})
        paths = [ROOT + "/parallel-" + str(i) for i in range(20)]
        locker = """import os,fcntl,hashlib,time
from pathlib import Path
paths=__PATHS__
Path('/tmp/agent-space').mkdir(exist_ok=True)
with open('/tmp/agent-space/file-operations.lock','a+b') as f:
 for p in paths:
  offset=int.from_bytes(hashlib.sha256(os.fsencode(os.path.realpath(p))).digest()[:8],'big') & ((1<<63)-2)
  fcntl.lockf(f,fcntl.LOCK_EX,1,offset)
 Path('__ROOT__/ready').touch()
 end=time.monotonic()+25
 while not Path('__ROOT__/release').exists() and time.monotonic()<end:time.sleep(.02)
""".replace("__PATHS__", repr(paths)).replace("__ROOT__", ROOT)
        good("write", {"path": ROOT + "/locker.py", "content": locker})
        good("bash", {"cmd": "rm -f " + ROOT + "/ready " + ROOT + "/release; python3 " + ROOT + "/locker.py </dev/null >/tmp/file-locker.log 2>&1 & while [ ! -f " + ROOT + "/ready ]; do sleep .02; done"})
        with concurrent.futures.ThreadPoolExecutor(max_workers=20) as pool:
            futures = [pool.submit(good, "write", {"path": p, "content": "value-" + str(i)}) for i,p in enumerate(paths)]
            probe = r"""import json,time
from pathlib import Path
end=time.monotonic()+5
while True:
 count=0
 for p in Path('/proc').glob('[0-9]*/cmdline'):
  try:
   a=p.read_bytes().split(b'\0')
   if len(a)>4 and a[1:3]==[b'-I',b'-c'] and b'Fixed MCP file operations' in a[3]:
    spec=json.loads(a[4]);count+=spec.get('path','').startswith('__ROOT__/parallel-')
  except (OSError,ValueError):pass
 if count>=4 or time.monotonic()>end:break
 time.sleep(.02)
print('simultaneous-file-processes='+str(count))
Path('__ROOT__/release').touch()
""".replace("__ROOT__",ROOT)
            try:
                result=good("bash", {"cmd":"python3 -c "+shlex.quote(probe)})
                self.assertIn("simultaneous-file-processes=4",json.dumps(result))
            finally:
                good("bash", {"cmd":"touch "+ROOT+"/release"})
            for f in futures:f.result(timeout=30)
        check="from pathlib import Path; p=Path("+repr(ROOT)+"); assert all((p/('parallel-'+str(i))).read_text()=='value-'+str(i) for i in range(20)); print('20-files-correct')"
        self.assertIn("20-files-correct",json.dumps(good("bash",{"cmd":"python3 -c "+shlex.quote(check)})))

    def test_02_parallel_same_path_edits_and_reads(self):
        path=ROOT+"/shared.txt"
        good("write",{"path":path,"content":"".join("<old%d>"%i for i in range(20))})
        good("bash",{"cmd":"ln -sf shared.txt "+ROOT+"/alias.txt"})
        with concurrent.futures.ThreadPoolExecutor(max_workers=12) as pool:
            list(pool.map(lambda i:good("edit",{"path":ROOT+"/alias.txt" if i%2 else path,"old_text":"<old%d>"%i,"new_text":"<new%d>"%i}),range(20)))
        read=json.dumps(good("read",{"path":path}),ensure_ascii=False)
        self.assertNotIn("<old",read)
        for i in range(20):self.assertIn("<new%d>"%i,read)

    def test_03_browser_work_does_not_block_basic_tools(self):
        browser=Client("desktop");browser.initialize()
        started=text(browser.tool("browser_act",{"actions":[{"do":"wait","secs":60}]}))
        self.assertEqual(started["state"],"running",started)
        before=time.monotonic()
        with concurrent.futures.ThreadPoolExecutor(max_workers=8) as pool:
            list(pool.map(lambda i:good("write",{"path":ROOT+"/during-browser-"+str(i),"content":"parallel"}),range(8)))
        good("bash",{"cmd":"echo bash-during-browser"})
        self.assertLess(time.monotonic()-before,6)
        while started["state"]=="running":
            started=text(browser.tool("browser_act",{"op":"wait","task_id":started["task_id"]}))
        self.assertEqual(started["state"],"succeeded",started)

    def test_04_lock_timeout_keeps_original_and_next_request_recovers(self):
        path=ROOT+"/timeout.txt"
        good("write",{"path":path,"content":"original"})
        script="import fcntl,hashlib,os,time;from pathlib import Path;f=open('/tmp/agent-space/file-operations.lock','a+b');offset=int.from_bytes(hashlib.sha256(os.fsencode("+repr(path)+")).digest()[:8],'big')&((1<<63)-2);fcntl.lockf(f,fcntl.LOCK_EX,1,offset);Path("+repr(ROOT+"/timeout-ready")+").touch();time.sleep(20)"
        good("bash",{"cmd":"rm -f "+ROOT+"/timeout-ready; python3 -c "+shlex.quote(script)+" </dev/null >/tmp/timeout-lock.log 2>&1 & echo $! > "+ROOT+"/timeout-lock.pid; while [ ! -f "+ROOT+"/timeout-ready ]; do sleep .02; done"})
        started=time.monotonic()
        try:
            result=call("write",{"path":path,"content":"must-not-commit"})
            self.assertTrue(result.get("isError"),result)
            self.assertLess(time.monotonic()-started,18)
        finally:
            good("bash",{"cmd":"kill $(cat "+ROOT+"/timeout-lock.pid)"})
        self.assertIn("original",json.dumps(good("read",{"path":path})))
        good("write",{"path":path,"content":"recovered"})
        self.assertIn("recovered",json.dumps(good("read",{"path":path})))

if __name__ == "__main__":
    unittest.main(verbosity=2)
