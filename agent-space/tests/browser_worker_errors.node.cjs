"use strict";
// Exercise the production worker boundary without starting Chromium. Only the
// observation subprocess and checkpoint filesystem are fault-injected.
const { test } = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
const root = "/opt/agent-space/desktop";
const worker = fs.readFileSync(root + "/browser-worker.cjs", "utf8");
const tools = require(root + "/browser-tools.cjs");
async function observeFailure(error, stderr) {
  const files = new Map();
  let calls = 0;
  const result = await new Promise((resolve, reject) => {
    const process = {
      env: { AGENT_TASK: JSON.stringify({ operation: "read" }), AGENT_RESULT: "/result" },
      execPath: "/node",
      stdout: { write: (_text, done) => done() },
      exit: (code) => resolve({ code, result: JSON.parse(files.get("/result")) }),
    };
    const requireInjected = (name) => {
      if (name === "node:fs") return {
        writeFileSync: (path, value) => files.set(path, value),
        renameSync: (from, to) => { files.set(to, files.get(from)); files.delete(from); },
      };
      if (name === "node:child_process") return {
        execFile: (_program, args, options, callback) => {
          assert.ok(options.timeout > 6000 && options.timeout <= 30000);
          assert.equal(args[0], root + "/observe-worker.cjs");
          calls++;
          queueMicrotask(() => callback(error, '{"page_id":"observed"}', stderr));
        },
      };
      if (name === "./timing.cjs") return require(root + "/timing.cjs");
      if (name === "./browser-tools.cjs") return {
        ...tools,
        run: async (_task, environment) => ({ value: await environment.observe("page") }),
      };
      if (name === root + "/artifacts.cjs") return {};
      return require(name);
    };
    try {
      vm.runInNewContext(worker, { require: requireInjected, process, __dirname: root,
        console: { error() {} }, Buffer, performance, setTimeout, clearTimeout });
    } catch (error) { reject(error); }
  });
  assert.equal(calls, 1, "observations must not silently retry a failed subprocess");
  return result;
}
test("observation timeout becomes a bounded public error", { timeout: 2000 }, async () => {
  const result = await observeFailure(Object.assign(Error("Command failed: /private/observe"), { code: 1 }),
    "The operation was aborted due to timeout");
  assert.equal(result.code, 1);
  assert.equal(result.result.error.code, "PAGE_UNRESPONSIVE");
  assert.match(result.result.error.message, /observe again/);
  assert.doesNotMatch(result.result.error.message, /private|Command failed/);
});
test("killed observation and non-timeout exit codes remain distinct", { timeout: 2000 }, async () => {
  const killed = await observeFailure(Object.assign(Error("killed"), { killed: true }), "");
  assert.equal(killed.result.error.code, "PAGE_UNRESPONSIVE");
  const failed = await observeFailure(Object.assign(Error("failed"), { code: 2 }), "socket unavailable " + "x".repeat(2000));
  assert.equal(failed.result.error.code, "BROWSER_UNAVAILABLE");
  assert.match(failed.result.error.message, /socket unavailable/);
  assert.ok(failed.result.error.message.length < 600);
});
test("successful observation keeps its result", { timeout: 2000 }, async () => {
  const result = await observeFailure(null, "");
  assert.equal(result.code, 0);
  assert.equal(result.result.value.page_id, "observed");
  assert.equal(result.result.error, undefined);
});

test('tabs view needs observation only, not a responsive page or a DOM snapshot', async () => {
  const result = await tools.run({operation:'snapshot',args:{view:'tabs'},page_id:'stale'}, {
    observe: async (id, view) => { assert.equal(id,null); assert.equal(view,'tabs'); return {tabs:[{page_id:'page',title:'Frozen',url:'about:blank'}]}; },
    page: () => { throw Error('tabs view must not attach to a page'); }
  });
  assert.deepEqual(Object.keys(result.value).sort(),['tabs']);
  assert.equal(result.value.tabs.length,1);
});

async function deadlineResult(phase, operation = "act", budget = 120000) {
  const files = new Map();
  let clock = 0, scheduled;
  const timers = new Map(); let next = 0;
  const result = await new Promise((resolve, reject) => {
    const process = {
      env: { AGENT_TASK: JSON.stringify({ operation, budget_ms: budget }), AGENT_RESULT: "/result" },
      execPath: "/node", stdout: { write: (_text, done) => done() },
      exit: code => resolve({ code, result: JSON.parse(files.get("/result")), scheduled }),
    };
    const injected = name => {
      if (name === "node:fs") return {
        writeFileSync: (p,v) => files.set(p,v),
        renameSync: (a,b) => {files.set(b,files.get(a));files.delete(a);},
      };
      if (name === "./timing.cjs") return require(root + "/timing.cjs");
      if (name === root + "/artifacts.cjs") return {};
      if (name === "./browser-tools.cjs") return { ...tools, run: async (_task, env) => {
        env.checkpoint({value:{completed:1,of:2,page_id:"p"},side_effects:"may_have_occurred"});
        env.armDeadline(30000, phase);
        const [fn, delay] = [...timers.values()][0]; scheduled=delay;
        clock=5000; assert.equal(env.remaining(), Math.min(30000,budget)-5000);
        clock=delay; fn();
        return new Promise(()=>{}); // expired work must not yield a successful result
      }};
      return require(name);
    };
    try { vm.runInNewContext(worker, {require:injected,process,__dirname:root,console:{error(){}},Buffer,
      performance:{now:()=>clock},setTimeout:(fn,ms)=>{timers.set(++next,[fn,ms]);return next;},clearTimeout:id=>timers.delete(id)}); }
    catch(e){reject(e);}
  });
  return result;
}
test('action watchdog shares remaining budget, keeps completed steps, and exits worker', async()=>{
  const v=await deadlineResult({step_number:2,do:"fill"},"act",12000);
  assert.equal(v.scheduled,12000);assert.equal(v.code,1);
  assert.equal(v.result.value.completed,1);assert.equal(v.result.failed_step.step_number,2);
  assert.equal(v.result.error.code,"TIMEOUT");assert.equal(v.result.side_effects,"may_have_occurred");
});
test('observation watchdog preserves successful actions and original failures', async()=>{
  const v=await deadlineResult({observation:true});assert.equal(v.scheduled,30000);assert.equal(v.code,0);
  assert.equal(v.result.value.completed,1);assert.equal(v.result.value.observation_error.code,"PAGE_UNRESPONSIVE");
  const failed=await deadlineResult({observation:true,failure:{step_number:2,do:"click",code:"STALE_REF",message:"stale"}});
  assert.equal(failed.code,1);assert.equal(failed.result.error.code,"STALE_REF");assert.equal(failed.result.failed_step.step_number,2);
});
test('backend policy distinguishes interaction and long operations',()=>{
  const t=require(root+'/timing.cjs');
  assert.equal(t.stepLimit({do:'fill'}),30000);
  assert.equal(t.stepLimit({do:'wait',secs:60}),Infinity);
  for(const step of [{do:'navigate'},{do:'download'},{do:'wait',text:'ready'},{do:'tab',action:'new',url:'about:blank'}]) assert.equal(t.stepLimit(step),60000);
});
