import fs from 'node:fs';
fs.mkdirSync('dist/licenses',{recursive:true});
for(const [name,path] of [['noVNC','node_modules/@novnc/novnc/LICENSE.txt'],['i18next','node_modules/i18next/LICENSE'],['qrcode','node_modules/qrcode/license']]) fs.copyFileSync(path,`dist/licenses/${name}.txt`);
fs.writeFileSync('dist/licenses/SOURCES.txt','noVNC 1.7.0 (MPL-2.0): https://github.com/novnc/noVNC/tree/v1.7.0\ni18next 26.4.3 (MIT): https://github.com/i18next/i18next\nqrcode 1.5.4 (MIT): https://github.com/soldair/node-qrcode\nModified desktop integration sources are in agent-space/desktop-ui; release inputs are recorded in runtime.json.\n');
