// Register existing workspace artifacts without copying their bytes.
'use strict';
const fs=require('node:fs'),path=require('node:path');
const directory='/workspace/agent-space/artifacts';
function publish(file){
 const real=fs.realpathSync(file);if(!real.startsWith('/workspace/'))throw Error('Artifact must be in /workspace');
 const stat=fs.statSync(real);if(!stat.isFile())throw Error('Artifact must be a regular file');
 const mime={'.png':'image/png','.jpg':'image/jpeg','.jpeg':'image/jpeg','.webp':'image/webp','.gif':'image/gif','.mp4':'video/mp4','.json':'application/json','.mhtml':'multipart/related','.txt':'text/plain'}[path.extname(real).toLowerCase()]||'application/octet-stream';
 return {path:real,name:path.basename(real),bytes:stat.size,mime_type:mime};
}
module.exports={publish,directory};
