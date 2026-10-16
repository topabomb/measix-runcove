#[path = "../../tests/common.rs"]
mod common;
use agent_engine::engine::{
    msb::MsbEngine, AgentEngine, AgentHandle, ManagedLaunch, ManagedService, VolumeSpec,
    WorkspaceOpts,
};
use futures::FutureExt;
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn exec(handle: &Arc<dyn AgentHandle>, command: &str) -> String {
    let output = handle
        .exec("bash", &["-lc", command], Some(Duration::from_secs(30)))
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0, "{}", output.stderr);
    output.stdout
}
async fn exchange(handle: &Arc<dyn AgentHandle>, port: u16, request: &[u8]) -> String {
    let mut tcp = handle.connect_guest_tcp(port).await.unwrap();
    tcp.writer.write_all(request).await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut buffer = [0; 8192];
        loop {
            let n = tcp.reader.read(&mut buffer).await.unwrap();
            if n == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..n]);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&bytes[..end]);
                let length = head.lines().find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                });
                if head.starts_with("HTTP/1.1 204")
                    || length.is_some_and(|length| bytes.len() >= end + 4 + length)
                {
                    break;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "incomplete HTTP response: {}",
            String::from_utf8_lossy(&bytes)
        )
    });
    String::from_utf8(bytes).unwrap()
}
#[tokio::test]
async fn guest_kernel_http_disconnect_and_enospc_keep_previous_file() {
    common::verify_helper_identity(
        std::path::Path::new(env!("CARGO_BIN_EXE_agent-space-webdav")),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap(),
        "webdav",
    );
    common::cleanup_test_sandboxes().await;
    let engine = MsbEngine::new();
    let name = "test-webdav-files";
    let opts = WorkspaceOpts {
        image: "nikolaik/python-nodejs:python3.12-nodejs22-slim".into(),
        cpus: 1,
        memory_mib: 256,
        shell: "/bin/bash".into(),
        volumes: vec![VolumeSpec::disk("/workspace", "ws", 128)],
        network_access: Default::default(),
        outbound_proxy: None,
        max_tcp_connections: None,
        max_udp_connections: None,
    };
    let handle = engine.create_sandbox(name, &opts).await.unwrap();
    let outcome=std::panic::AssertUnwindSafe(async {
        exec(&handle,r#"mkdir -p /opt/agent-space/bin /opt/agent-space/etc
printf '%s' '{"schemaVersion":1,"root":"/workspace","prefix":"/u/alice/dav/","lockTimeoutSecs":1800}' > /opt/agent-space/etc/webdav.json
printf sentinel > /workspace/file
printf secret > /tmp/outside-secret
ln -s /tmp/outside-secret /workspace/escape"#).await;
        let launch = ManagedLaunch::Webdav {
            host_binary: env!("CARGO_BIN_EXE_agent-space-webdav").into(),
            owner: "spc_vm".into(),
            config: serde_json::json!({"schemaVersion":1,"root":"/workspace","prefix":"/u/alice/dav/","lockTimeoutSecs":1800}),
        };
        let instance = handle.start_managed(&launch).await.unwrap();
        let port = instance.port;
        let adopted = engine.get_sandbox(name).await.unwrap();
        let reused = adopted.start_managed(&launch).await.unwrap();
        assert_eq!(reused, instance, "Host reconnect must reuse one compatible instance");
        assert_eq!(adopted.list_managed(ManagedService::Webdav).await.unwrap().len(), 1);
        let received=exchange(&handle,port,b"GET /u/alice/dav/file HTTP/1.1\r\nHost: files\r\nConnection: close\r\n\r\n").await;
        assert!(received.contains("200 OK") && received.ends_with("sentinel"),"{received}");
        let received=exchange(&handle,port,b"PUT /u/alice/dav/file HTTP/1.1\r\nHost: files\r\nContent-Length: 7\r\nConnection: close\r\n\r\nupdated").await;
        assert!(received.contains("204 No Content"),"{received}");
        assert_eq!(exec(&handle,"cat /workspace/file").await,"updated");
        let received=exchange(&handle,port,b"GET /u/alice/dav/escape HTTP/1.1\r\nHost: files\r\nConnection: close\r\n\r\n").await;
        assert!(received.contains("403 Forbidden"),"{received}");
        let script=r#"python3 - <<'PY'
import http.client, socket, time
port=PORT
# Disconnect without completing a chunked request.
s=socket.create_connection(('127.0.0.1',port))
s.sendall(b'PUT /u/alice/dav/file HTTP/1.1\r\nHost: files\r\nTransfer-Encoding: chunked\r\n\r\n7\r\npartial\r\n')
s.close()
for _ in range(100):
    import os
    if not [name for name in os.listdir('/workspace/.agent-space/uploads/vm_test') if name != 'boot-id']: break
    time.sleep(.02)
assert open('/workspace/file','rb').read()==b'updated'
# Verify metadata and quota on the guest's real filesystem.
import xml.etree.ElementTree as ET
c=http.client.HTTPConnection('127.0.0.1',port,timeout=15)
c.request('PUT','/u/alice/dav/dated',b'date',{'X-OC-Mtime':'946684800'})
r=c.getresponse();assert r.status==201 and r.getheader('X-OC-Mtime')=='accepted';r.read()
assert int(os.stat('/workspace/dated').st_mtime)==946684800
xml=b'<D:propertyupdate xmlns:D="DAV:"><D:set><D:prop><D:getlastmodified>Sun, 01 Jan 2023 00:00:00 GMT</D:getlastmodified></D:prop></D:set></D:propertyupdate>'
c.request('PROPPATCH','/u/alice/dav/dated',xml);r=c.getresponse();assert r.status==207 and b'200 OK' in r.read()
assert int(os.stat('/workspace/dated').st_mtime)==1672531200
xml=b'<D:propfind xmlns:D="DAV:"><D:prop><D:quota-used-bytes/><D:quota-available-bytes/></D:prop></D:propfind>'
c.request('PROPFIND','/u/alice/dav/',xml,{'Depth':'1'});r=c.getresponse();assert r.status==207
quotas=[(int(x.find('.//{DAV:}quota-used-bytes').text),int(x.find('.//{DAV:}quota-available-bytes').text)) for x in ET.fromstring(r.read())]
assert all(q==quotas[0] for q in quotas)
stat=os.statvfs('/workspace');assert quotas[0][1]==stat.f_bavail*stat.f_frsize
with open('/workspace/copy-source','wb') as source: os.posix_fallocate(source.fileno(),0,16*1024*1024)
open('/workspace/copy-target','wb').write(b'copy-original')
c.close()
# Fill the actual usable capacity, leaving enough room for staging metadata.
stat=os.statvfs('/workspace')
with open('/workspace/fill','wb') as filler:
    os.posix_fallocate(filler.fileno(),0,stat.f_bavail*stat.f_frsize-8*1024*1024)
# A 64 MiB PUT cannot fit in the remaining 8 MiB.
c=http.client.HTTPConnection('127.0.0.1',port,timeout=15)
c.putrequest('PUT','/u/alice/dav/file')
c.putheader('Content-Length',str(64*1024*1024))
c.putheader('Connection','close')
c.endheaders()
try:
    for _ in range(1024): c.send(b'x'*(64*1024))
except (BrokenPipeError,ConnectionResetError): pass
r=c.getresponse()
assert r.status==507, (r.status,r.read())
c.close()
for _ in range(100):
    if not [name for name in os.listdir('/workspace/.agent-space/uploads/vm_test') if name != 'boot-id']: break
    time.sleep(.02)
assert open('/workspace/file','rb').read()==b'updated'
assert not [name for name in os.listdir('/workspace/.agent-space/uploads/vm_test') if name != 'boot-id']
# COPY must not destroy the old target when its temporary copy fills the disk.
c=http.client.HTTPConnection('127.0.0.1',port,timeout=15)
c.request('COPY','/u/alice/dav/copy-source',headers={'Destination':'/u/alice/dav/copy-target'})
r=c.getresponse();assert r.status==507,(r.status,r.read());r.read();c.close()
assert open('/workspace/copy-target','rb').read()==b'copy-original'
assert os.stat('/workspace/copy-source').st_size==16*1024*1024
assert not [name for name in os.listdir('/workspace/.agent-space/uploads/vm_test') if name != 'boot-id']
print('disconnect and ENOSPC preserved original; no staging files remain')
PY"#.replace("PORT",&port.to_string()).replace("vm_test", &instance.instance_id);
        assert!(exec(&handle,&script).await.contains("ENOSPC preserved original"));
        // Crash with a leftover same-volume request file. A new owner must prove
        // that the old process ended, then clean only its marked instance dir.
        let stale = format!("/workspace/.agent-space/uploads/{}/{}", instance.instance_id, "00112233-4455-6677-8899-aabbccddeeff");
        exec(&handle, &format!("printf partial > {stale}; kill -KILL {}", instance.pid)).await;
        tokio::time::timeout(Duration::from_secs(3), async {
            while adopted.managed_alive(&instance).await.unwrap() { tokio::task::yield_now().await; }
        }).await.unwrap();
        let replacement = adopted.start_managed(&launch).await.unwrap();
        assert_ne!(replacement.instance_id, instance.instance_id);
        exec(&handle, &format!("test ! -e /workspace/.agent-space/uploads/{}; test ! -e /tmp/agent-space/{}", instance.instance_id, instance.instance_id)).await;
        assert_eq!(exec(&handle,"cat /workspace/file").await,"updated");
        adopted.stop_managed(&replacement).await.unwrap();
        assert!(adopted.list_managed(ManagedService::Webdav).await.unwrap().is_empty());
        let mut invalid = launch.clone();
        if let ManagedLaunch::Webdav { config, .. } = &mut invalid { config["schemaVersion"] = 99.into(); }
        assert!(adopted.start_managed(&invalid).await.is_err());
        assert!(adopted.list_managed(ManagedService::Webdav).await.unwrap().is_empty(), "failed startup must remove its owned record");
    }).catch_unwind().await;
    engine.remove_sandbox(name).await.unwrap();
    engine.remove_volume(&format!("{name}-ws")).await.unwrap();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
