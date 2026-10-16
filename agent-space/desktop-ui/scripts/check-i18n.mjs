import fs from 'node:fs';
const zh=JSON.parse(fs.readFileSync('src/locales/zh-CN.json'));const en=JSON.parse(fs.readFileSync('src/locales/en.json'));
if(JSON.stringify(Object.keys(zh).sort())!==JSON.stringify(Object.keys(en).sort()))throw Error('Locale keys differ');
for(const v of Object.values(en))if(/[\u4e00-\u9fff]/.test(v))throw Error('Untranslated English string');
console.log('Locale parity: '+Object.keys(en).length+' keys');
