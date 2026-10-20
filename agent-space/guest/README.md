# Agent Space managed guest services

Your files, code and outputs belong in `/workspace`. Applications that you start
are yours to manage. Agent Space owns its `static-http`, `webdav` and desktop helpers.

`/tmp` is a bounded RAM-backed filesystem, not part of the workspace disk.
Server examples allow 512 MiB within each VM memory budget; Standalone examples use a 256 MiB container tmpfs.
Use `/workspace` for large downloads, archives and build outputs.

- `/opt/agent-space/bin` contains distributed helpers; `/opt/agent-space/etc`
  contains Host-generated configuration. Do not edit them or start extra copies.
- `/tmp/agent-space/<instance-id>` contains the current instance identity,
  configuration snapshot, readiness and bounded diagnostic log. It is not user
  data and is not restored as runtime state after a VM or container restart.
- Desktop spaces use `/opt/agent-space/desktop` for the matched Node workers
  distributed with the Rust helper. Do not replace workers or start another
  browser. The one managed browser is shared by automation and the visible VNC
  desktop. The tools ensure it exists when needed.
- `/workspace/.agent-space/desktop/profile` preserves the browser profile;
  `/workspace/agent-space/artifacts` holds screenshots, downloads and
  other outputs. Keep the profile private and intact while the browser runs.
- Human takeover controls browser automation only. bash/read/write/edit and workspace file transfers remain available. File tools allow four concurrent operations per workspace, serialize conflicting canonical paths, and include queue/lock waits in their deadline. bash calls share a serial queue.
  Passive observation remains available. Only accepted human input renews the
  60-second control lease; five minutes without work closes the desktop while
  keeping the profile and workspace. A later browser call wakes it.
- Use `browser_snapshot` for interactive refs and floating panels, `browser_read`
  for text or structured data, and `browser_act` for ordered browser/desktop actions.
  `browser_act(op="wait"|"cancel", task_id=...)` controls all browser and screenshot tasks.
- Every page image covers the full rendered document. `browser_snapshot(image=true)`
  includes ref annotations; `screen_observe(view="page")` provides plain images,
  timed sequences and optional saved PNG originals. `view="desktop"` captures native
  windows too. Coordinates use pixels in the returned image.
- Uploads and downloads use workspace paths. Saved downloads and screenshot files
  survive later task failure or cancellation. No JavaScript tool is provided.
- `read` returns numbered text by default. Set `multimodal: true` to view JPEG, PNG, WebP or GIF; line-range parameters are ignored for images. Animations show the first frame.
- `/workspace/.agent-space/uploads` contains incomplete DAV uploads on the same
  disk as your files. Do not modify or manually clean it. It is hidden from DAV.
- `web_publish` publishes a selected file, directory or running HTTP port. Use
  a unique name for each publication; get/list inspect it and unpublish closes it.
  Basic username is `web` plus two digits; use the returned username and six-digit password, or send the password in
  `X-Publish-Key` for programmatic access. Credentials are managed by the Host.
- Server URLs use `/u/{owner}/pub/{name}/`; Standalone uses `/pub/{name}/` on the service
  origin. Port applications must support this base path (`X-Forwarded-Prefix`);
  absolute asset URLs and application redirects are not rewritten. Publish trusted
  content only: different paths and Basic realms do not isolate same-origin scripts.
- On Server, publishing normally keeps the VM alive for 10 minutes; repeated publish of
  the same name updates its target and extends keepalive, preserving URL and credentials; failed preparation keeps the previous target. The deadline is not a URL expiry:
  publications remain while this VM runs and end when it stops. Closing one
  publication does not disable DAV, delete files or terminate your applications.
  Disabling/deleting a Server workspace or stopping its VM ends guest processes. Standalone accepts keep_alive_minutes for tool compatibility but has no VM idle shutdown or URL TTL; Docker owns container lifetime.
- When a managed service fails, report its instance ID and diagnostic error to
  the Host owner. Do not use `pkill`, edit credentials or try to repair ownership
  records. Host configuration and account state remain authoritative.

On Standalone, commands, browser actions, saved screenshots and file operations
run as UID/GID1001. The Runcove service and helper controllers run as root and own
private credentials and runtime records. Install application dependencies in
project-local environments, such as a Python virtual environment or a local npm
project. System packages belong in the deployment image; Agent tasks have no
runtime root or unrestricted sudo access.

On Server, tools, Chromium, desktop apps and internal observers all use root
inside the VM with HOME=/root; the core service is outside the VM. Chromium uses
--no-sandbox and relies on the VM isolation boundary. Standalone keeps all user
execution at UID/GID1001 with HOME=/home/desktop and its browser sandbox. Managed-service
conventions prevent accidental interference; they do not isolate guest helpers
from root commands inside a VM.
