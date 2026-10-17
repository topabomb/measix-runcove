# Measix Runcove 浏览器与云桌面合同

本页是两种部署共用的浏览器、任务、控制权和文件行为基准。下文 `/u/{username}/...`、镜像选择及 StateStore 是已实现 Server 的装配；Standalone 固定单空间的入口为 `/mcp`、`/desktop/`、`/pub/{name}/` 和可选 `/dav/`，使用单实例访问凭据；最终镜像资格以[验证记录](validation-records.md)为准。

构建与运行只使用本仓库源码及锁定依赖。迁移见[运维手册](operations.md)，共享核心与适配边界见[架构](architecture.md)，实测范围见[验证记录](validation-records.md)。

## 镜像决定能力

`[workspace].image` 仍接受自定义镜像字符串；桌面使用明确的预设对象：

```toml
[workspace]
image = { preset = "runcove-desktop" }
cpus = 2
memory_mib = 3072
workspace_size_mib = 8192
tmpfs_size_mib = 512
shell = "/bin/bash"
exec_timeout_secs = 30
```

预设要求动态状态模式。Admin 创建用户也接受 `image`：字符串或 `{"preset":"runcove-desktop"}`；省略使用部署默认值。创建时将 `kind/reference/preset/workerContract` 固定在 schema3 用户记录中，Admin 返回 `image` 和派生的 `browser`。更改部署默认值只影响新空间。已有根盘不热换镜像，不通过镜像名称猜测能力。

| 空间选择 | 工具目录 | 桌面入口 |
|---|---|---|
| 自定义字符串，包括与预设相同的 OCI 地址 | bash、read、write、edit、web_publish | 不提供 |
| runcove-desktop 预设 | 原五工具加 browser_observe、screen_observe、browser_run、browser_task | 提供 |

初始化和工具发现不启动 VM。目录与调用按同一持久镜像能力鉴权；绕过目录直接调用浏览器工具也被拒绝。镜像 lock 记录真实 linux/amd64 OCI manifest digest，发布包离线携带该镜像；不能将 Docker config ID 当作 manifest，也不自动回退任意 tag。

## 运行职责

Rust Host 继续拥有账号、鉴权、MCP、空间生命周期、任务监督、控制权和 HTTP/WS。Rust `agent-space-desktop` 是第三种固定受管 helper，监听 guest 回环；它拥有 X11/TigerVNC/LXQt、唯一可见 Chromium 及浏览器 worker。Node 使用官方 Playwright 连接这个浏览器；Python 只负责有界执行及桌面截图。没有第二个 MCP 服务或 bash 工具。

helper 二进制内嵌匹配的五份 JS 源码，启动时原子安装到 `/opt/agent-space/desktop/`。helper 摘要覆盖 worker 内容，避免 Host 升级后继续使用镜像中旧 worker。只接管通过 boot/start/inode/摘要校验的实例；不能按名称或裸 PID 清理。

| 路径 | 内容及归属 |
|---|---|
| `/opt/agent-space/bin/agent-space-desktop` | Host 注入的固定 helper |
| `/opt/agent-space/desktop/` | 与 helper 匹配的 Node worker |
| `/tmp/agent-space/<instance-id>/` | 身份、有限日志、UUID 任务结果；不跨 VM 重启恢复 |
| `/workspace/.agent-space/desktop/profile/` | 持久浏览器 profile，包含敏感登录信息 |
| `/workspace/agent-space/artifacts/` | 截图、下载、MHTML、大返回值等产物 |

默认桌面 1440×900；VNC 5901、CDP 9222、helper 随机端口仅限 guest 回环，并禁止作为 web_publish 端口发布。Chromium/LXQt/worker 使用 UID1001，bash 保留现有身份。guest root 命令仍可修改这些目录，内部协作约束不构成对抗 root 的安全边界。

公开目录是默认落盘位置，Agent仍可在其他工作区路径组织文件；不自动创建 `/workspace/agent-space/README.md`。桌面启动时把旧默认产物目录中的普通文件移到新目录，同名冲突以UUID前缀保留两份，未知子目录/符号链接留在原处。仅将Chromium原默认下载路径改到新位置，用户自选路径保留。旧绝对产物链接不提供永久别名，迁移后从产物列表取得当前路径。

## 浏览器与截图工具

工具及参数说明由 `tools/list` 完整提供，客户端不依赖 initialize.instructions。桌面空间共九个工具，自定义非桌面空间仍为五个；`screen_observe` 与三个 browser 工具使用同一镜像能力校验，不能绕过目录直接调用。

| 工具 | 参数与默认值 | 返回 |
|---|---|---|
| browser_observe | page_id 可选；view=`dom` 或 ax；recent_tasks_limit=0，0..10 | 当前页、可用标签、已知健康、控制模式，以及一种语义视图 |
| screen_observe | view 必填 desktop/page；page_id 可选；count=1，1..8；interval_ms=1000，非负；save_original=false；save_dir 可选 | 完成结果或 task_id；终态含帧说明和按捕获顺序排列的 MCP 图像块；保存原图时附 files |
| browser_run | code 必填，1..65536 UTF-8 bytes；page_id 可选；timeout_secs=60，1..600 | 脚本返回值和显式输出；超过首次8秒等待则返回任务句柄 |
| browser_task | task_id 必填；action=`wait` 或 cancel；wait_secs=20，0..20 | 浏览器任务或截图序列的进度、最终结果或取消状态 |

`browser_observe` 可直接传 `{}`，不导航、不聚焦，人工接管期间也可读取。省略 page_id 或传 null 时优先聚焦标签页，否则选择已有标签页；显式无效 ID 返回 PAGE_NOT_FOUND 和 tabs，不替换目标。dom 返回可见正文、控件及当前值；ax 单独返回可访问性树，二者不同时返回。默认不附带任务历史；指定 recent_tasks_limit 后按 Host 接受时间从新到旧列出摘要，旧 page/desktop 视图同样保留显式请求的历史。工具中的控制信息只投影模式 AI/HUMAN/TRANSITION/BLOCKED，工作台的人类输入权限不混入 Agent 观察结果。

### 截图与连续观察

```json
{"view":"desktop"}
```

`screen_observe` 的 page_id、count、interval_ms、save_original、save_dir 均为可选参数，工具参数说明明确标注 Optional。desktop 捕获完整桌面及原生窗口，忽略 page_id；page 捕获选定标签页的可视区域，不滚动、不聚焦、不建立 Playwright 附着，也不先读取 DOM/AX。序列只在首次选择目标，同一标签可以导航；目标关闭则失败并保留前面已完成的帧，不替换为其他页面。旧 `browser_observe` 的 page/desktop 视图兼容转到这一实现，返回截图任务结构。

第一帧准备好即捕获，后续按捕获起始时间保持至少 interval_ms 间隔；处理较慢时顺延，不并行补拍，零表示连续捕获。最多8帧，整个序列含准备、等待和图片处理最多600秒；无法容纳的时间安排在接受前拒绝。超过首次8秒等待后用 browser_task 查询或取消。运行中只回进度；终态返回 view/requested_count/captured_count/frames，frames 含 index/image_id/elapsed_ms/width/height/resized，图片只放在对应顺序的 MCP image 块。失败、取消和超时仍保留已完成帧及具体错误。

桌面截图只要求可用显示服务，Chromium/CDP 崩溃或卡住不阻断它，也不为截图重启现有桌面。页面截图失败明确报错，由 Agent 决定是否改观察桌面。截图不占浏览器修改槽，不跨帧间隔持有桌面锁，人工控制及浏览器任务运行期间仍可观察；取消序列只停止剩余观察。观察调用刷新桌面工作活动，轮询不续期。

序列的返回图片编码合计最多2 MiB，按请求帧数分配单帧预算。对同一次捕获做有界压缩：JPEG 质量80、70，再按需把最长边缩到1600、1280；初始最长边不超过2000，不重新捕获。仍超预算时明确 IMAGE_BUDGET_EXCEEDED，已有帧保留，不静默丢图或无限降质。默认不落盘；只有显式启用保存时才保留原图文件。

需要下载原图时设置 `save_original=true`，可选 `save_dir` 指定空间内可写的绝对目录，默认 `/workspace/agent-space/artifacts`，缺失目录自动创建。未启用保存却指定目录属于参数错误。每帧捕获的原始 PNG 字节按 `screen-<10位Base62随机串>.png` 原子保存，不覆盖已有文件；观察返回的图片仍按上述预算优化。终态 `files` 按捕获顺序包含 `frame_index/image_id/path/width/height/bytes/mime_type`，尺寸和字节数对应原图。可通过现有工作台文件流或 DAV 下载工作区内的文件，也可用 `read(multimodal=true)` 读取。保存失败返回 `SCREEN_SAVE_FAILED` 并保留可显示的当前帧；取消、超时或后续失败保留已保存文件及其路径。

```json
{"view":"desktop","count":3,"interval_ms":1000,"save_original":true,"save_dir":"/workspace/screenshots"}
```

桌面 image_id 的有效期从实际捕获算起60秒，并绑定捕获前后相同的前台窗口；无法确认窗口时仍返回图片，但拒绝据此进行指针动作。坐标使用返回图片的像素，服务自动换算到原桌面尺寸。长序列中较早帧可能在返回时已过期，应重新观察后再操作。

### JavaScript 执行与显式输出

`browser_run` 注入 `page`、`context`、`desktop`、`output`、`artifacts`，接受 async 函数或带 await 的函数体；模块用 `await import(...)` 导入。artifacts 是默认可写目录路径，其他目标也须具有写权限。普通运行没有标签页且无原生窗口时创建空白页，不创建第二个浏览器或 context。选择 page_id 不自动聚焦；优先复用已有页，仅有并行页面需求时新建标签，用完关闭自己创建的临时页。

上传可用 `locator.setInputFiles(path)`，或先登记 `page.waitForEvent('filechooser')`，触发后调用 `chooser.setFiles(path)`。下载同样先登记事件再触发：

```javascript
const pending = page.waitForEvent('download');
await page.getByRole('link', {name:'Download', exact:true}).click();
const file = await output.download(await pending);
return {path:file.path, bytes:file.bytes};
```

`output.file(path)` 只登记现有工作区文件的元数据，不复制或自动预览；`output.image(bufferOrCapture)` 显式返回 PNG/JPEG/WebP；`output.download(download)` 保存并登记下载；`output.saveMhtml(page,path)` 保存并登记 MHTML。`desktop.screenshot()` 返回 Capture，使用 `await output.image(await desktop.screenshot())` 显示。已登记输出原子检查点保存，后续脚本报错、超时或取消仍返回可用产物和日志。成功只表示脚本完成，不解释返回值中业务字段的含义；不自动采集最终 DOM、AX 或截图。

指针接口为 `desktop.click({image_id,x,y,button?})`、`desktop.drag({image_id,x,y,to_x,to_y})`、`desktop.scroll({image_id,x,y,delta})`；其他接口为 `desktop.type(text)`、`desktop.press(key)`、`desktop.setWindowMode('normal'|'maximized'|'fullscreen')`。原生对话框或页面无响应时不新建 Playwright 附着，改用 desktop helpers。参数形状错误给出真实签名，截图过期或前台窗口改变给出 STALE_OBSERVATION。

### 任务、错误与输出预算

期限从 Host 接受起计，覆盖准备和执行；等待不延长期限。guest 监督 Node worker，超时或取消时按进程身份先 TERM 后 KILL 并核对后代；HTTP 等待者断开不释放实际工作所有权，也不重放动作。修改清理确认后才能移交控制权，无法确认时进入 BLOCKED，禁止新修改。

运行中返回 task_id/state/phase/remaining_secs；终态加非空的 value/files/logs/output_warning/captures 或截图帧说明，图片单独放 MCP image 块，不重复进文字或 structuredContent。成功省略空字段及内部清理标记；失败使用 `error:{code,message}`，任务执行失败有 `isError=true`，修改任务保留有意义的 side_effects，BUSY 附 active_task_id。参数错误使用协议 InvalidParams。未知错误保留有界原始原因，去除控制序列后最多2048字符；定位器错误保留目标和主因，不重复返回重试日志。

| 输出 | 限制 |
|---|---|
| 工具结果传输 | 每次12 MiB，计入 Base64、JSON 和协议封装预留；超限明确报错，不截断图片 |
| DOM / AX | controls≤60、text≤4000、AX≤100行/6000字符、tabs≤50；显式 truncated |
| JS 返回值 | JSON≤32 KiB；超出保存为 JSON 产物，不可序列化返回 warning |
| browser_run 图片 / 文件 | ≤4张，图片原始编码合计≤8 MiB；文件≤16项，只回元数据 |
| worker stdout/stderr | 各≤32 KiB / 500行，有截断标记 |
| 截图坐标缓存 | 最近≤32张，生命周期从实际捕获起计 |
| 任务缓存 | 完成后最多30分钟，Host 重启失效；网站副作用不回滚 |

TASK_NOT_FOUND、超时、断连和失败不能证明网站尚未收到请求，应观察当前状态和产物后再决定下一步。bash 不纳入 browser_task。接入方须允许至少12 MiB的工具结果与 SSE 数据帧；外部 Agent、模型或网关是否完整接收图片需独立联调，本仓库的验证不代表外部系统已经兼容。

## read 的图片读取

read 保留 path/offset/limit，新增 `multimodal`，默认 false；自定义非桌面空间同样支持，不新增文件查看工具。

| 输入 | 行为 |
|---|---|
| 文本文件，任意 multimodal | 保持行号、offset/limit、默认500行及现有文本截断，不施加图片源大小限制 |
| 图片，默认关闭 | MULTIMODAL_REQUIRED，提示设置 multimodal=true |
| 支持图片，multimodal=true | 文件与尺寸说明加一个标准 MCP image 块 |
| 图片同时指定 offset/limit | 协议 InvalidParams，说明行范围只适用于文本；显式传1/500也拒绝 |
| 不支持、损坏或超限媒体 | 具体执行错误，isError=true；不退回文本读取 |

按内容识别 JPEG、PNG、WebP、GIF，扩展名不决定处理方式；BM、II* 等短 ASCII 前缀不单独作为媒体依据。GIF、动态 WebP、APNG 只取首个合成帧并注明；不调用额外模型做 OCR 或图片描述。源文件≤10 MiB，解码画布≤1600万像素，返回最长边≤2000像素、不放大，输出≤4,500,000字节，每次一张。完整解码前检查尺寸、像素与解码预算；无需变换时尽量保留原编码，需要方向修正、抽帧或缩放时再编码，保留透明度，有界压缩失败明确报错。

文件脚本复用真实路径、同一打开句柄、共享读锁和有界二进制读取；沿用原文件准入、四路队列、期限、取消及活动租约。内部图片报文有独立预算，包含 Base64/JSON；50 KiB 默认文本上限不变，媒体报文截断、缺失或解析失败直接报错。Rust 共享模块负责识别、校验、方向、缩放和编码，无需 Workspace 安装 Pillow。每个处理进程的图片管线最多并行两项，等待及处理计入请求期限；取消或超时不会提前释放仍在执行的同步解码/编码所占许可及活动租约，实际处理结束后回收。Server、Standalone 共用实现。

## 控制权及生命周期

默认 AI；多人可同时观看，只有一个桌面页面会话可接管。服务端按完整 RFB 消息过滤键盘、鼠标、剪贴板及屏幕调整输入；其他观看者仍接收图像。接管只取消当前浏览器自动化任务并等待清理；bash/read/write/edit及工作台文件传输不参与桌面控制门禁，人工接管时仍可执行。此控制权是浏览器工具与VNC输入的协调规则，不是对任意bash/用户进程操作桌面的安全隔离。只有接受的输入续期60秒，轮询、ping、被拒输入和观看不续期。交还、持有人连接断开或无动作到期后回 AI，并释放已按下的键和鼠标按钮。

浏览器工具、显式唤醒和实际人工输入刷新桌面活动；任务轮询、状态/健康查询和持续被动观看不刷新。无活动300秒且无任务时关闭桌面，profile/工作文件保留；后来调用工具或点击唤醒重新打开。启动不传 about:blank URL，优雅 Browser.close 保留标签。VM 空闲回收仍归原 SessionManager，不增加另一套 VM 生命周期。

禁用空间会撤回旧Session及其DesktopHub缓存；重新启用并签发新凭据后，桌面绑定新的运行所有者和控制权，继续使用原磁盘/profile。删除空间同时清除桌面缓存与镜像投影，不保留已退役的运行所有者。

## 工作台路由与身份

工作台顶部在登录前后持续显示路由中的目标用户名，使用品牌下方的次级文字；长名称省略显示，悬停或聚焦可查看全称。登录表单显示完整用户名，窄屏允许换行。名称仅呈现已有路由身份，不查询额外用户信息，也不代替Token与目标用户的认证校验。

| 路由 | 职责 |
|---|---|
| `/u/{username}/desktop/` | Runcove 工仓云桌面，中文/英文，无刷新切换 |
| `desktop/api/login`、session、logout | 当前空间 MCP Token 换取/撤销页面会话 |
| `desktop/api/control`、health、viewer | 控制权、真实桌面进程健康、显式唤醒 |
| `desktop/api/artifacts`、files/{path} | 产物列表及受认证的工作区文件流 |
| `desktop/ws` | WS→guest TCP/RFB，服务端输入门禁 |

使用同一 Host 监听地址，不开宿主 VNC/CDP 端口。页面资源/API/WS 都从当前页面相对路径生成；外部可增加并剥离前缀，不配置桌面 public_url/base_path/IP白名单。无尾斜线入口用相对重定向。桌面 Cookie 的登录 Origin 与 nonce 保护不依赖代理转发的 Host；原 MCP/DAV/发布的 origin 合同继续沿用。

登录使用现有空间 MCP Token，不使用 DAV token。服务端只保留 space/keyId 绑定，页面随机会话 Cookie 为 HttpOnly、SameSite=Strict、12小时绝对有效；HTTPS Origin 使用 Secure。写 API/WS 还验证随机 nonce，拒绝跨站请求；每次请求重验空间状态和凭据，撤销也关闭连接。Host 重启页面会话失效，UI明确要求重新登录。Token 不写 URL、localStorage、日志或 guest；配置面板仅在当前页面内存可显式展示/复制/二维码分享，应按凭据对待。

工作台文件 GET/HEAD/PUT 复用现有 DAV helper 的受限文件系统与流式代理。上传全程持有VM活动租约，不占用浏览器控制门禁，原子提交且禁止覆盖；断开不提交半文件。下载支持 Range。1GiB文件通过真实HTTP流传输，模型/MCP不拼接原始字节。浏览器网站上传引用 guest 中的 `/workspace/...` 路径；这与本机上传到空间是两个连续步骤。

文件面板不列 profile，但空间拥有者通过原文件工具有权读工作区。Web发布只允许可信内容；同源页面不能依赖 Cookie Path 实现脚本隔离。外网TLS和网关由部署方负责，入口要求见[部署指南](deployment.md)。

## 验证入口

Rust 测试与原真实 VM 回归见 AGENTS.md。新增独立资格测试需要测试 Host、测试凭据文件和已加载镜像：

```bash
export AGENT_SPACE_BROWSER_TEST_DIR=/path/to/independent-qualification
python3 tests/test_bash_vm.py
python3 tests/test_browser_vm.py
python3 tests/test_desktop_files_vm.py
python3 tests/test_browser_lifecycle_vm.py # 真实300秒，串行且勿操作该空间
MSB_HOME=/path/to/isolated-msb python3 tests/test_migration_cli.py
AGENT_SPACE_BROWSER_TEST_MSB_HOME=/path/to/isolated-msb python3 tests/test_failed_probe.py
python3 tests/test_exec_owner.py
cd desktop-ui && npm ci && npm run build && npm test
```

同一空间的浏览器脚本与人工输入按控制权互斥；基础文件工具、bash和工作台文件流允许与桌面操作同时进行。破坏性生命周期测试仍须使用独立空间，不能与其他测试争用VM启停。

这些测试明确区分脚本 MCP、浏览器/UI、真实 VM、Docker候选、实际模型Agent和第三方网站。fixture通过不能代替Grok登录/付费生成成功；实测和未验证边界必须登记，不自动切换现有服务。
