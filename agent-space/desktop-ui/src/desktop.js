import RFB from '@novnc/novnc';
import QRCode from 'qrcode';
import {t,formatBytes} from './i18n';

const basePath=new URL('./',location.href).pathname.replace(/\/$/,'');
const serviceUrl=path=>basePath+'/api'+path;
const $=id=>document.getElementById(id),clipboardLimit=16*1024;
// Display the target already named in the route; authentication still verifies it.
const desktopUser=decodeURIComponent(basePath.match(/\/u\/([^/]+)\/desktop$/)?.[1]||'');
for(const id of ['desktopUser','loginUser']){$(id).textContent=desktopUser;$(id).title=desktopUser;if(!desktopUser)$(id).parentElement.hidden=true;}
let humanIdle=60,canInput=false,nonce=null,configToken='',rfb=null,connected=false,connecting=null,authenticated=false,phase='checking',control='UNKNOWN',controlIssue=null,remaining=0,confirmed=false,controlBusy=false,polling=false,mutation=0,panel=null,fit=true,retryTimer=null,retries=0,toastTimer=null,uploadRequest=null,suspended=false;
let mcpData=null,mcpLoad=0,mcpRevealed=false;
function say(text){$('message').textContent=text;clearTimeout(toastTimer);toastTimer=setTimeout(()=>$('message').textContent='',8000);}
function clipSay(text){$('clipboardStatus').textContent=text;}
function clearClipboard(){ $('clipboardSend').value='';$('clipboardReceive').value='';$('copyClipboard').disabled=true;clipSay(t("panel_cleared_system_clipboards_are_unchanged")); }
function render(){
  const writable=connected&&confirmed&&control==='HUMAN'&&canInput&&!controlBusy&&phase==='live';
  document.body.classList.toggle('human',writable);document.body.classList.toggle('live',connected&&phase==='live');
  if(rfb)rfb.viewOnly=!writable;
  $('connection').textContent=({checking:t("checking_connection"),login:t("not_signed_in"),connecting:t("connecting_desktop"),live:t("desktop_connected"),reconnecting:t("reconnecting"),offline:t("desktop_disconnected"),idle:t("desktop_sleeping"),denied:t("access_denied")})[phase];
  $('state').textContent=controlBusy||control==='TRANSITION'?t("transferring_control"):control==='BLOCKED'?t("transfer_failed_input_locked"):!confirmed?t("input_locked"):control==='HUMAN'?(canInput?t("you_are_controlling"):t("someone_else_is_controlling_view_only")):t("ai_control_view_only");
  $('state').title=controlIssue||'';
  if(control==='BLOCKED')$('message').textContent=t("control_transfer_failed")+(controlIssue||t("task_cleanup_is_unconfirmed_retry_taking_control"));
  $('take').querySelector('span').textContent=control==='BLOCKED'?t("retry_control"):t("take_control");
  $('take').hidden=control==='HUMAN';$('give').hidden=control!=='HUMAN'||!canInput;
  $('take').disabled=$('give').disabled=!authenticated||!confirmed||!connected||controlBusy||control==='TRANSITION'||control==='BLOCKED'||phase!=='live';
  $('lease').hidden=!writable||remaining>10;$('lease').textContent=remaining+t("s_until_ai_control");
  $('sendClipboard').disabled=$('escapeKey').disabled=$('addressKey').disabled=!writable;
  $('upload').disabled=!authenticated||!!uploadRequest;
  $('uploadHint').textContent=t("upload_to_the_workspace_then_select_it");
  $('mcpToggle').disabled=$('filesToggle').disabled=$('clipboardToggle').disabled=$('logout').disabled=!authenticated;
  $('overlay').hidden=phase==='live';$('loginForm').hidden=phase!=='login';$('reconnect').hidden=!['offline','idle'].includes(phase);
  const copy={checking:[t("checking_connection"),t("checking_desktop_access_please_wait")],login:[t("connect_to_runcove_desktop"),t("sign_in_with_your_space_token_view")],connecting:[t("connecting_desktop"),t("preparing_the_desktop_please_wait")],reconnecting:[t("desktop_connection_interrupted"),t("input_locked_reconnecting_and_checking_control")],offline:[t("connection_unavailable"),t("check_the_network_or_service_then_reconnect")],idle:[t("desktop_sleeping"),t("workspace_files_and_browser_profile_are_preserved")],denied:[t("request_origin_rejected"),t("reopen_the_page_from_the_current_service")]};
  if(copy[phase])[$('overlayTitle').textContent,$('overlayText').textContent]=copy[phase];
  $('reconnect').textContent=phase==='idle'?t("wake_desktop"):t("reconnect");
}
async function api(path,method='GET',body,timeout=10000){
  const abort=new AbortController(),timer=setTimeout(()=>abort.abort(),timeout);
  try{const response=await fetch(serviceUrl(path),{method,signal:abort.signal,headers:{...(body?{'Content-Type':'application/json'}:{}),...(nonce?{'X-Desktop-Nonce':nonce}:{})},body:body?JSON.stringify(body):undefined});const value=await response.json();if(!response.ok){const e=Error(value.message||value.error||t("request_failed"));e.status=response.status;e.code=value.error;throw e;}return value;}
  catch(e){if(e.name==='AbortError')throw Error(t("request_timed_out_rechecking_state_avoid_duplicate"));throw e;}finally{clearTimeout(timer);}
}
function dropConnection(){const old=rfb;rfb=null;connected=false;old?.disconnect();clearClipboard();}
function fail(e){
  confirmed=false;
  if(e.status===401||e.status===403){clearTimeout(retryTimer);retryTimer=null;authenticated=false;configToken='';$('mcpToken').value='';control='UNKNOWN';phase=e.status===401?'login':'denied';dropConnection();closePanel(false);say(e.status===401?t("session_expired_enter_your_token_again"):t("request_origin_rejected_2"));}
  else{if(phase!=='idle')phase='offline';say(e.message==='Failed to fetch'?t("cannot_reach_the_service_check_the_network"):e.message);}
  render();
}
async function state(){const version=mutation;const value=await api('/control');if(version!==mutation)return;const previous=control;authenticated=true;confirmed=true;control=value.control;canInput=!!value.can_input;controlIssue=value.control_error||null;humanIdle=value.human_idle_secs??60;$('idlePolicy').textContent=t('control_returns_to_ai_after_60_seconds',{seconds:humanIdle});remaining=value.human_remaining_secs??0;if(connected&&!controlBusy)phase='live';render();if(previous==='HUMAN'&&control==='AI'&&!controlBusy)say(t("control_returned_to_ai_desktop_is_view"));}
function scheduleReconnect(){
  if(!authenticated||connecting||retryTimer||phase==='idle'||phase==='denied')return;
  const delay=[2000,5000,10000][retries++];if(delay===undefined){phase='offline';render();return;}
  phase='reconnecting';render();retryTimer=setTimeout(async()=>{retryTimer=null;try{await connect(false);}catch(e){fail(e);scheduleReconnect();}},delay);
}
async function connect(wake){
  if(connecting)return connecting;if(connected){await state();return;}
  clearTimeout(retryTimer);retryTimer=null;
  connecting=(async()=>{
    await state();
    if(wake)await api('/viewer','POST',{},45000);
    else{const h=await api('/health');if(!h.desktop){phase='idle';render();return;}}
    phase='connecting';render();

    const current=new RFB($('screen'),location.protocol.replace('http','ws')+'//'+location.host+basePath+'/ws?nonce='+encodeURIComponent(nonce));rfb=current;
    current.scaleViewport=fit;current.resizeSession=false;current.viewOnly=true;current.showDotCursor=true;current.focusOnClick=true;
    current.addEventListener('clipboard',event=>{if(rfb!==current||!connected||current.viewOnly)return;const text=event.detail.text;if(new TextEncoder().encode(text).length>clipboardLimit){$('clipboardReceive').value='';$('copyClipboard').disabled=true;clipSay(t("remote_text_exceeds_16_kib_transfer_a"));return;}$('clipboardReceive').value=text;$('copyClipboard').disabled=!text;clipSay(t("remote_text_received_copy_locally_or_select"));});
    await new Promise((resolve,reject)=>{
      const timer=setTimeout(()=>{if(rfb===current){rfb=null;connected=false;}current.disconnect();reject(Error(t("desktop_connection_timed_out_retry")));},15000);
      current.addEventListener('connect',()=>{clearTimeout(timer);if(rfb!==current)return;connected=true;resolve();},{once:true});
      current.addEventListener('disconnect',()=>{clearTimeout(timer);if(rfb!==current)return;rfb=null;connected=false;confirmed=false;clearClipboard();phase='reconnecting';render();reject(Error(t("desktop_disconnected_2")));scheduleReconnect();},{once:true});
    });
    await state();phase='live';retries=0;render();
  })();
  try{await connecting;}finally{connecting=null;}
}
async function changeControl(mode){
  if(controlBusy||!connected)return;controlBusy=true;++mutation;render();let completed=false;
  try{await api('/control','POST',{mode},45000);++mutation;await state();if(!connected)throw Error(t("desktop_disconnected_recheck_control_ownership"));completed=true;}
  catch(e){fail(e);}
  finally{controlBusy=false;++mutation;try{if(authenticated)await state();}catch(e){fail(e);}render();if(connected&&confirmed&&control==='HUMAN')rfb.focus();if(completed&&confirmed)say(control==='HUMAN'?t("mouse_and_keyboard_enabled_control_returns_to",{seconds:humanIdle}):t("control_returned_to_ai_desktop_is_view"));}
}
const panels={mcp:['mcpPanel',()=>t("mcp_connection"),'mcpToggle'],clipboard:['clipboardPanel',()=>t("clipboard"),'clipboardToggle'],files:['artifactsPanel',()=>t("files_artifacts"),'filesToggle'],display:['displayPanel',()=>t("display_controls"),'displayToggle']};
function closePanel(focus=true){const trigger=panel&&$(panels[panel][2]);clearMcp();panel=null;$('drawer').hidden=true;for(const [,,id] of Object.values(panels))$(id).setAttribute('aria-expanded','false');if(focus){if(connected&&control==='HUMAN')rfb.focus();else trigger?.focus();}}
function openPanel(name){if(panel===name){closePanel();return;}clearMcp();panel=name;$('drawer').hidden=false;for(const [key,[id,,trigger]] of Object.entries(panels)){$(id).hidden=key!==name;$(trigger).setAttribute('aria-expanded',String(key===name));}$('panelTitle').textContent=panels[name][1]();if(name==='files')refreshArtifacts().catch(fail);if(name==='mcp')loadMcp();}
function mcpTab(qr){$('mcpConfigView').hidden=qr;$('mcpQrView').hidden=!qr;for(const [id,selected] of [['mcpConfigTab',!qr],['mcpQrTab',qr]]){$(id).setAttribute('aria-selected',String(selected));$(id).tabIndex=selected?0:-1;}if(qr&&mcpData)$('mcpQr').hidden=!mcpData.qr_data_url;}
function discardMcp(){++mcpLoad;mcpData=null;mcpRevealed=false;$('mcpJson').value='';$('mcpQr').removeAttribute('src');$('mcpQr').hidden=true;$('mcpLocal').hidden=true;$('copyMcpAddress').disabled=$('copyMcpConfig').disabled=$('revealMcp').disabled=true;renderMcp();}
function clearMcp(){discardMcp();$('mcpAddress').value=$('mcpStatus').textContent='';$('generateMcp').disabled=true;mcpTab(false);}
function renderMcp(){if(mcpData){const preview=structuredClone(mcpData.config);if(!mcpRevealed)preview.runcove.headers.Authorization='Bearer <hidden>';$('mcpJson').value=JSON.stringify(preview,null,2);}$('revealMcp').setAttribute('aria-pressed',String(mcpRevealed));const text=mcpRevealed?t("hide_token"):t("show_token");$('revealMcp').title=text;$('revealMcp').setAttribute('aria-label',text);}
async function loadMcp(target){
 discardMcp();let value=target||new URL('../mcp',location.href).href;
 if(!/^https?:\/\//i.test(value))value=location.protocol+'//'+value;
 try{const address=new URL(value);if(!['http:','https:'].includes(address.protocol)||address.username||address.password||address.hash)throw Error('bad address');
 const token=$('mcpToken').value||configToken||'<token>';const config={runcove:{type:'http',url:address.href,headers:{Authorization:'Bearer '+token}}};
 const qr=await QRCode.toDataURL(JSON.stringify(config),{margin:2,width:240});
 mcpData={config,qr_data_url:qr};$('mcpAddress').value=address.href;$('mcpQr').src=qr;
 $('mcpLocal').hidden=!['localhost','127.0.0.1','[::1]'].includes(address.hostname);
 $('generateMcp').disabled=$('copyMcpAddress').disabled=$('copyMcpConfig').disabled=$('revealMcp').disabled=false;$('mcpStatus').textContent='';renderMcp();if(!$('mcpQrView').hidden)$('mcpQr').hidden=false;
 }catch(e){$('generateMcp').disabled=false;$('mcpStatus').textContent=t("generation_failed_check_the_address_and_retry");}
}
$('mcpAddress').oninput=()=>{discardMcp();$('generateMcp').disabled=!$('mcpAddress').value.trim();$('mcpStatus').textContent=t("address_changed_regenerate_the_configuration");};
$('mcpToken').oninput=()=>loadMcp($('mcpAddress').value);
$('mcpConnectionForm').onsubmit=e=>{e.preventDefault();if(!$('generateMcp').disabled)loadMcp($('mcpAddress').value.trim());};
async function copyMcp(text,field){try{if(window.isSecureContext&&navigator.clipboard?.writeText){await navigator.clipboard.writeText(text);say(t("copied_locally"));return;}}catch{}const temporary=document.createElement('textarea');temporary.value=text;temporary.style='position:fixed;opacity:0;pointer-events:none';document.body.append(temporary);temporary.select();let copied=false;try{copied=document.execCommand('copy');}catch{}temporary.remove();if(copied){say(t("copied_locally"));return;}if(field==='mcpJson'){mcpRevealed=true;renderMcp();}$(field).focus();$(field).select();say(t("automatic_copy_unavailable_text_selected_press_ctrl"));}
$('mcpConfigTab').onclick=()=>mcpTab(false);$('mcpQrTab').onclick=()=>mcpTab(true);
for(const id of ['mcpConfigTab','mcpQrTab'])$(id).onkeydown=e=>{if(['ArrowLeft','ArrowRight','Home','End'].includes(e.key)){e.preventDefault();const qr=e.key==='End'||e.key!=='Home'&&id==='mcpConfigTab';mcpTab(qr);$(qr?'mcpQrTab':'mcpConfigTab').focus();}};
$('revealMcp').onclick=()=>{mcpRevealed=!mcpRevealed;renderMcp();};
$('copyMcpConfig').onclick=()=>{if(mcpData)copyMcp(JSON.stringify(mcpData.config,null,2),'mcpJson');};$('copyMcpAddress').onclick=()=>{if(mcpData)copyMcp(mcpData.config.runcove.url,'mcpAddress');};
const size=formatBytes;
async function refreshArtifacts(){
  $('refreshArtifacts').disabled=true;
  try{const value=await api('/artifacts');$('artifactFiles').replaceChildren();if(!value.files.length){const p=document.createElement('p');p.className='hint';p.textContent=t("no_artifacts_yet");$('artifactFiles').append(p);}
    for(const file of value.files){const row=document.createElement('div');row.className='file-row';const info=document.createElement('div');info.className='file-info';const title=document.createElement('span');title.textContent=file.name;const meta=document.createElement('small');meta.textContent=size(file.bytes);info.append(title,meta);const a=document.createElement('a');a.href=serviceUrl('/files/'+file.path.replace(/^\/workspace\//,'').split('/').map(encodeURIComponent).join('/'));a.download=file.name;a.setAttribute('aria-label',t("download")+file.name);a.title=t("download")+file.name;a.innerHTML='<svg class="icon" aria-hidden="true"><use href="#i-download"/></svg>';row.append(info,a);$('artifactFiles').append(row);}
  }finally{$('refreshArtifacts').disabled=false;}
}
$('loginForm').onsubmit=async event=>{event.preventDefault();if(controlBusy)return;controlBusy=true;$('login').disabled=true;try{configToken=$('token').value;const logged=await api('/login','POST',{token:configToken,cookie_path:basePath+'/'});nonce=logged.nonce;$('token').value='';authenticated=true;retries=0;await connect(true);}catch(e){fail(e);}finally{controlBusy=false;$('login').disabled=false;render();}};
$('reconnect').onclick=async()=>{if(connecting)return;retries=0;const wake=phase==='idle';phase='connecting';render();try{await connect(wake);}catch(e){fail(e);scheduleReconnect();}};
$('take').onclick=()=>changeControl('HUMAN');$('give').onclick=()=>changeControl('AI');
$('mcpToggle').onclick=()=>openPanel('mcp');$('clipboardToggle').onclick=()=>openPanel('clipboard');$('filesToggle').onclick=()=>openPanel('files');$('displayToggle').onclick=()=>openPanel('display');$('closePanel').onclick=()=>closePanel();$('refreshArtifacts').onclick=()=>refreshArtifacts().catch(fail);
$('sendClipboard').onclick=()=>{try{if(!connected||!rfb||rfb.viewOnly||controlBusy)throw Error(t("take_control_before_sending_clipboard_text"));const text=$('clipboardSend').value;if(new TextEncoder().encode(text).length>clipboardLimit){clipSay(t("text_exceeds_16_kib_shorten_it_or"));return;}rfb.clipboardPasteFrom(text);rfb.focus();clipSay(t("clipboard_sent_press_ctrl_v_in_the"));}catch(e){if(e.status)fail(e);else say(e.message);}};
$('copyClipboard').onclick=async()=>{const field=$('clipboardReceive');if(!field.value)return;if(window.isSecureContext&&navigator.clipboard?.writeText){try{await navigator.clipboard.writeText(field.value);clipSay(t("copied_locally"));return;}catch{}}field.focus();field.select();let copied=false;try{copied=document.execCommand('copy');}catch{}clipSay(copied?t("copied_locally"):t("text_selected_press_ctrl_c_to_copy"));};
$('clearClipboard').onclick=clearClipboard;
$('upload').onclick=()=>{if(!$('upload').disabled)$('file').click();};
$('file').onchange=async()=>{
  const file=$('file').files[0];if(!file||uploadRequest)return;
  try{await state();if(file.size>4*1024**3)throw Error(t("file_exceeds_the_4_gib_upload_limit"));
    $('uploadProgress').value=0;$('uploadProgress').hidden=false;$('cancelUpload').hidden=false;$('uploadStatus').textContent=t("uploading")+file.name;
    await new Promise((resolve,reject)=>{const xhr=new XMLHttpRequest();uploadRequest=xhr;render();xhr.open('PUT',serviceUrl('/files/'+encodeURIComponent(file.name)));xhr.setRequestHeader('X-Desktop-Nonce',nonce);xhr.timeout=330000;xhr.upload.onprogress=e=>{if(e.lengthComputable){$('uploadProgress').value=e.loaded/e.total*100;$('uploadStatus').textContent=t("uploading")+Math.round(e.loaded/e.total*100)+'% · '+file.name;}};xhr.onload=()=>{if(xhr.status===412)return reject(Error(t("a_file_with_this_name_exists_rename")));let v;try{v=xhr.responseText?JSON.parse(xhr.responseText):{};}catch{return reject(Error(t("invalid_upload_response_check_workspace_files_before")));}if(xhr.status<200||xhr.status>=300){if(xhr.status===412)return reject(Error(t("a_file_with_this_name_exists_rename")));const e=Error(v.error==='FILE_EXISTS'?t("a_file_with_this_name_exists_rename"):v.message||v.error);e.status=xhr.status;return reject(e);}resolve(v);};xhr.onerror=()=>reject(Error(t("upload_interrupted_check_workspace_files_before_retrying")));xhr.ontimeout=()=>reject(Error(t("upload_timed_out_check_workspace_files_before")));xhr.onabort=()=>reject(Error(t("upload_cancelled_check_the_workspace_if_it")));xhr.send(file);});
    $('uploadStatus').textContent=t("uploaded")+file.name+' · '+size(file.size);$('downloadPath').value=file.name;
  }catch(e){$('uploadStatus').textContent=e.message;if(e.status===401||e.status===403)fail(e);}
  finally{uploadRequest=null;$('cancelUpload').hidden=true;$('uploadProgress').hidden=true;$('file').value='';render();}
};
$('cancelUpload').onclick=()=>uploadRequest?.abort();
$('download').onclick=()=>{const p=$('downloadPath').value.trim().replace(/^\/workspace\//,'');if(!p){say(t("enter_a_workspace_file_path"));return;}const a=document.createElement('a');a.href=serviceUrl('/files/'+p.split('/').map(encodeURIComponent).join('/'));a.download=p.split('/').pop();a.click();};
function setFit(value){fit=value;if(rfb)rfb.scaleViewport=value;$('fit').setAttribute('aria-pressed',String(value));$('actual').setAttribute('aria-pressed',String(!value));}
$('fit').onclick=()=>setFit(true);$('actual').onclick=()=>setFit(false);
$('fullscreen').onclick=async()=>{try{if(document.fullscreenElement)await document.exitFullscreen();else await document.documentElement.requestFullscreen();}catch{say(t("fullscreen_is_unavailable_maximize_the_browser_window"));}};
document.addEventListener('fullscreenchange',()=>{const text=document.fullscreenElement?t("exit_fullscreen"):t("enter_fullscreen");$('fullscreen').title=text;$('fullscreen').setAttribute('aria-label',text);});
function sendKey(address){try{if(!connected||!rfb||rfb.viewOnly||controlBusy)return;rfb.focus();if(address){rfb.sendKey(0xffe3,'ControlLeft',true);try{rfb.sendKey(0x6c,'KeyL');}finally{rfb.sendKey(0xffe3,'ControlLeft',false);}}else rfb.sendKey(0xff1b,'Escape');}catch(e){fail(e);}}
$('logout').onclick=async()=>{if(!authenticated)return;try{await api('/logout','POST',{});authenticated=false;confirmed=false;nonce=null;configToken='';$('mcpToken').value='';control='UNKNOWN';phase='login';clearTimeout(retryTimer);retryTimer=null;dropConnection();closePanel(false);render();say(t("signed_out_of_this_desktop_page_other"));}catch(e){fail(e);}};
$('escapeKey').onclick=()=>sendKey(false);$('addressKey').onclick=()=>sendKey(true);
setInterval(async()=>{if(suspended||polling||controlBusy||connecting||!authenticated)return;polling=true;try{await state();}catch(e){fail(e);}finally{polling=false;}},2000);
window.addEventListener('pagehide',()=>{suspended=true;clearTimeout(retryTimer);retryTimer=null;dropConnection();closePanel(false);});
window.addEventListener('pageshow',async event=>{if(!event.persisted)return;suspended=false;try{await connect(false);}catch(e){fail(e);scheduleReconnect();}});
render();try{const logged=await api('/session');nonce=logged.nonce;await state();await connect(true);}catch(e){if(e.status===401){authenticated=false;phase='login';render();}else{fail(e);scheduleReconnect();}}

window.addEventListener('localechange',()=>{render();if(panel)$('panelTitle').textContent=panels[panel][1]();if(panel==='files')refreshArtifacts().catch(fail);});
