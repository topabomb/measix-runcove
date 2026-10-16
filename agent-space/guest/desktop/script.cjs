'use strict';
const vm=require('node:vm');
// Compile only: detecting a function must never execute submitted browser actions.
function body(code){
  const expression=code.trim().replace(/;+\s*$/,'');
  if(/^(?:async\s*)?(?:function\b|\([^)]*\)\s*=>|[A-Za-z_$][\w$]*\s*=>)/.test(expression)){
    try{new vm.Script('('+expression+')');return 'return await ('+expression+')(page,context,desktop,output,artifacts);';}catch{}
  }
  return code;
}
function compile(code){
  const source=body(code);
  const AsyncFunction=Object.getPrototypeOf(async function(){}).constructor;
  return new AsyncFunction('page','context','desktop','output','artifacts',source);
}
module.exports={compile};
