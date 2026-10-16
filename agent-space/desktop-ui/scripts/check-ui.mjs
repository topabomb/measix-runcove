import fs from 'node:fs';
const html=fs.readFileSync('index.html','utf8'),source=fs.readFileSync('src/desktop.js','utf8');
const ids=[...html.matchAll(/\bid="([^"]+)"/g)].map(m=>m[1]);
if(new Set(ids).size!==ids.length)throw Error('Duplicate DOM id');
for(const [,id] of source.matchAll(/\$\(['"]([^'"]+)['"]\)/g))if(!ids.includes(id))throw Error('Missing DOM id: '+id);
console.log('UI DOM id contract passed');
