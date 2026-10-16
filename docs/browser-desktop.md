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
| runcove-desktop 预设 | 原五工具加 browser_observe、browser_run、browser_task | 提供 |

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

## 三个浏览器工具

工具及参数描述全部英文，完整放在 `tools/list`，客户端丢弃 initialize.instructions 仍可使用。`view`和`action`描述明确要求JSON字符串（如`"dom"`、`"wait"`）；省略可选项使用默认值。没有 open/close 工具、script_path、强制 observation_id 或新的选择器语言。

| 工具 | 参数与默认值 | 返回 |
|---|---|---|
| browser_observe | page_id 可选；view=`dom`，可选 page/desktop；recent_tasks_limit=3，1..10 | 当前页、标签、健康、受限 DOM/AX；指定视图的 PNG；最近任务摘要和控制状态 |
| browser_run | code 必填，1..65536 UTF-8 bytes；page_id 可选；timeout_secs=60，1..600 | 8秒内完成则最终结果；否则 task_id/state/phase/remaining_secs |
| browser_task | task_id 必填；action=`wait` 或 cancel；wait_secs=20，0..20 | 只查询/取消该任务；运行中简短状态，成功保留结果、产物和最终观察，失败仅保留错误及已有结果 |

`browser_observe` 可直接传 `{}`，不导航、不聚焦，人工接管期间也可读取。省略page_id或传JSON null时优先聚焦标签页，否则选择已有标签页；显式无效ID报错并给出修正方法，不转而操作其他页面。desktop视图不使用page_id，不因网页ID失效阻断桌面截图。dom 是默认语义快照；page 只截网页；desktop 截整个桌面，包括浏览器工具栏、原生对话框。最近任务按 Host 接受时间从新到旧筛选，包括成功、失败及运行中任务，30分钟后过期。

`browser_run` 注入 `page`、`context`、`desktop`、`output`、`artifacts`，接受 async 函数或带 await 的函数体。`page_id` 通常省略；选择指定标签页不自动聚焦。普通运行没有标签页且无原生窗口时创建空白页，不创建第二个浏览器或 context。优先复用已有标签页导航，仅需同时保留多个页面时使用context.newPage()；bringToFront()用于显式切到前台。自己创建的临时标签页用完关闭，保留原有标签页和未完成工作。优先等待元素、URL或事件，部分失败后先确认当前状态再重试。页面已无响应或原生对话框在前台时，避免新的 Playwright 附着；这时 page/context 不可用，使用 desktop helpers 恢复后再观察。

```javascript
await page.getByRole('button', {name:'Import'}).click();
await page.locator('input[type=file]').setInputFiles('/workspace/article.md');
return {url:page.url()};
```

没有 input[type=file] 时，先监听 filechooser 再点击并 `chooser.setFiles(path)`。下载同样先监听，最后仅保存一次：

```javascript
const pending = page.waitForEvent('download');
await page.getByRole('button', {name:'Download', exact:true}).click();
const file = await output.download(await pending);
return {path:file.path, bytes:file.bytes};
```

`output.file(path)` 登记现有工作区文件，不复制同体积文件；`output.saveMhtml(page,path)` 保存 MHTML。`output.image(PNG Buffer)` 或 `output.image(await desktop.screenshot())` 输出图片。桌面动作使用 `desktop.click({image_id,x,y})`、drag、scroll、type(text)、press(key)，窗口可 setWindowMode(normal/maximized/fullscreen)。坐标绑定截图像素及前台窗口，60秒过期，不能将缩放后的 VNC CSS 坐标直接传入。

## 任务、超时和输出

期限从 Host 接受任务起计，包含启动准备；等待工具不延长期限。guest 独立监督 Node worker，超时/取消先 TERM 后 KILL，并核对进程身份及后代；Host HTTP 等待者断开不释放仍在执行的所有权，也不重放动作。修改清理最多8秒，最终观察最多5秒；控制权必须等修改清理确认才可移交。无法证明清理时进入 BLOCKED，禁止新修改，由服务管理者核对并恢复该空间运行时。

终态有 task_id/state，加非空的 value/files/images/logs/output_warning/captures/observation。成功不夹带空 error、side_effects 或内部 cleanup_confirmed；失败保留实际错误和 `side_effects=may_have_occurred|not_started`，清理不确定时保留 false 证明。成功时的最终 observation 对应执行开始时选定的标签页；若已关闭，则返回剩余标签与健康状态，不替换为其他页面正文。最终观察保留最近3个任务的简短摘要；失败、超时和取消不自动采集或附带完整页面观察，按需显式观察当前页面。顶层不再重复任务列表。错误原因明确时直接提炼并提供必要的英文修正引导，否则去除ANSI控制序列后截断至2048字符，保留头尾并标记 `[error truncated]`；不增加诊断参数或错误字段。已有结果和产物不受错误文本上限影响；脚本中途未写入结果文件的产物不保证返回。不会返回全部历史日志或任务。

| 输出 | 限制 |
|---|---|
| DOM / AX | controls≤60、text≤4000、AX≤100行/6000字符、tabs≤50；显式 truncated |
| JS 返回值 | JSON≤32KiB；超出落为 JSON 产物；不可序列化报告 warning |
| 每任务图片 | ≤4张，合计≤8MiB；作为 MCP image blocks，文本不重复 base64 |
| 每任务文件 | ≤16项，只回元数据及路径 |
| worker stdout/stderr | 各≤32KiB / 500行，有截断标记；不含每轮累计日志 |
| 截图坐标元数据 | 保存最近≤32张，输出仅包含本轮实际发出的截图 |
| 任务缓存 | 完成后30分钟；Host重启句柄消失，网站副作用不回滚 |

TASK_NOT_FOUND、超时、断连和失败不能证明网站尚未收到请求。先观察页面/已有任务，再决定下一步；bash 不纳入 browser_task。

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
