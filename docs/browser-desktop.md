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
| runcove-desktop 预设 | 原五工具加 browser_snapshot、browser_act、browser_read、screen_observe | 提供 |

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
| `/workspace/agent-space/artifacts/` | 截图和下载产物 |

默认桌面 1440×900；VNC 5901、CDP 9222、helper 随机端口仅限 guest 回环，并禁止作为 web_publish 端口发布。Chromium、LXQt、内部观察、固定浏览器操作、截图保存及产物列表与 bash、文件工具使用相同的空间文件身份：Server 在 VM 内统一使用 root，Standalone 使用 UID1001，后者的 root 控制器保持私有。Server 的 Chromium 使用 `--no-sandbox`，由 VM 隔离空间；Standalone 保持原有非 root 浏览器沙箱，详见[身份合同](architecture.md#执行身份与环境管理)。guest root 命令仍可修改这些目录，内部协作约束不构成对抗 root 的安全边界。

Standalone 的 Agent 可管理项目和用户态依赖，系统依赖由部署镜像提供；root 服务控制进程与用户任务的权限边界见[架构说明](architecture.md#执行身份与环境管理)。

公开目录是默认落盘位置，Agent仍可在其他工作区路径组织文件；不自动创建 `/workspace/agent-space/README.md`。桌面启动时把旧默认产物目录中的普通文件移到新目录，同名冲突以UUID前缀保留两份，未知子目录/符号链接留在原处。仅将Chromium原默认下载路径改到新位置，用户自选路径保留。旧绝对产物链接不提供永久别名，迁移后从产物列表取得当前路径。

## 浏览器与截图工具

工具及参数说明由 `tools/list` 完整提供，客户端不依赖 initialize.instructions。桌面空间共九个工具，自定义非桌面空间仍为五个；`screen_observe` 与三个 browser 工具使用同一镜像能力校验，不能绕过目录直接调用。

工具说明中的 connected Runcove space 指当前连接绑定的命令执行环境和文件系统；`/workspace` 是其中的持久工作目录。desktop 指该空间的可见桌面，browser 指桌面上人和 Agent 共用的浏览器，tab 指其中的标签页；page 指标签页当前加载的文档，step 指 actions 中的一步。两组工具共享文件路径，路径按所属空间解释；与本地同名工具使用相同路径字符串不代表同一文件。截图帧由 image_id 标识，浏览器或截图任务由 task_id 标识，分别与标签页的 page_id 区分。

| 工具 | 参数与默认值 | 返回 |
|---|---|---|
| browser_snapshot | view=page/tabs/tasks；page_id/ref 可选；image=false | 当前页精简语义树与 ref、控制模式；异常时附健康信息；view=tabs 单独列页，view=tasks 单独列任务；可附标注整页图 |
| browser_act | op=execute；actions 1..20；page_id 可选；then=diff；执行期限由后端管理 | 完成步数、批末观察差异、弹出页、下载及失败位置；超过首次30秒返回 task_id |
| browser_read | page_id/ref 可选；默认正文 max_chars=4000，1..20000；或 query；或 extract=table/links/fields | 有界正文、搜索或结构化提取，包含 iframe 内容及截断标记 |
| screen_observe | view 必填 page/desktop；page_id 可选；count=1，1..8；interval_secs=1；save_original=false；save_dir 可选 | 单图或定时序列，终态按顺序返回图像；保存时附 files |

四个工具替代原有浏览器入口，不接受 JavaScript、选择器或另一套脚本接口。每次观察/操作使用短生命周期 worker 与连接，任务、控制权和浏览器生命周期仍由既有 Rust owner 负责。

### 说明与参数类型

工具描述说明操作对象、用途和关键输出；默认值、单位、互斥关系和条件必填项放在对应参数中。工具集合针对当前连接的 Runcove 空间；上传路径、截图保存目录和 file URL 另在参数处说明文件归属。控制权、焦点保持及故障恢复的详细行为见下文，不在每个工具描述中重复。

`browser_act.inputSchema.properties.actions.items` 引用同一 schema 内的 `$defs.ActStep`；其 `oneOf` 包含16个动作，每个分支具有动作 description、固定的 do 值、字段 description 和 required。说明随工具目录返回，不依赖外部讲解。

`tools/list` 的 schema 提供类型、枚举、数值和集合范围；description 补充取值来源、运行时默认值及组合规则。只披露有意义的执行默认值，不返回 `default: null`。条件必填不能仅看顶层 required：例如 execute 必须带 actions，wait/cancel 必须带 task_id；每个 actions 步骤的 do 和必要字段另有 required。

| 参数类别 | 类型与取值规则 |
|---|---|
| ref / to_ref | string；从 browser_snapshot、browser_read 或 browser_act 的结果原样复制，绑定实际元素，不自行拼造 |
| page_id | string；来自 browser_snapshot(view='tabs') 的 tabs[].page_id |
| image_id | string；来自 browser_snapshot 或 screen_observe 的图片结果；scroll(target=desktop) 只接受桌面图 |
| task_id | string；来自浏览器或截图任务结果 |
| op / action | string enum；顶层 op 为 execute/wait/cancel；tab 步骤的 action 为 new/close/focus |
| do | string const；选择下表的一种动作，每种动作有自己的字段和必填规则 |
| then / extract / view | string enum；then 为 diff/snapshot，extract 为 table/links/fields；snapshot 的 view 为 page/tabs/tasks，screen_observe 的 view 为 page/desktop |
| button / target / mode | string enum；button 为 left/right/middle，target 为 page/desktop；navigate.mode 为 open/back/forward/reload，window.mode 为 normal/maximized/fullscreen |
| image、exact、double、submit、checked、accept、save_original | boolean；使用 true/false。动作中的 text 则为 string |
| actions / paths | array；分别为 1..20 个动作对象、1..20 个空间内绝对路径字符串 |
| 坐标 x/y/to_x/to_y | number；返回图片内的非负像素坐标，左上角为原点，有效上界由图片尺寸决定 |
| 计数、时间和滚动量 | integer；各参数 schema 和说明列出范围、单位与默认值。interval_secs 及各步等待还受整个任务期限约束 |

### 观察、读取和定位

`browser_snapshot({})` 默认只返回语义信息，减少重复传图。省略 page_id 或传 null 时选择聚焦标签页，否则选择可用页；显式无效 ID 返回 PAGE_NOT_FOUND，不换目标；用 view=tabs 查询可用页面。选择标签不等于聚焦；需要桌面输入时先用 tab/focus。没有页面时不补建空白页；用 tab/new 明确创建。观察、读取、截图可在 HUMAN 模式执行，不获取修改槽。

ref 绑定实际元素和所属文档。节点存续时跨调用稳定；同名节点替换、导航或 iframe 重建后旧 ref 失效，不能靠名称/位置重新命中。snapshot、动作反馈、query、fields 返回的 ref 均可直接用于动作；ref 子树读取不会扩大到其他表单。snapshot/read 工具描述明确目标为当前连接 Runcove 空间的内置浏览器。snapshot 的旧 diff/text/recent_tasks_limit 参数静默忽略且不再公开；snapshot 始终返回当前完整的有界语义树（ref 可限定子树）。act 的 then=diff 与同一 MCP 连接中该页面最近一次无 ref 快照或动作反馈比较；缺少可靠基线、换文档、重排或截断时返回完整结构。read 和 ref 子树快照不更新基线。view=tasks 返回空间内浏览器与桌面任务列表，再用 act 的 wait/cancel 查询或取消指定任务。控制信息只含 AI/HUMAN/TRANSITION/BLOCKED，不混入工作台观看者权限。

`view=tasks` 直接读取当前空间任务管理器，不读取页面、不创建任务、不更新观察基线，忽略 page_id/ref/image。返回顶层 control、tasks；每项含 task_id/tool/description/state/started_at，时间沿用UTC `MM-DD HH:mm:ssZ`。进行中任务优先，各组按创建时间倒序；最多10条，有更多记录时附 truncated=true，空列表为 tasks=[]。页面观察不再附带 recent_tasks。任务来源为 act整批动作、snapshot页面或tabs观察、read、screen_observe（连续截图仍是一个任务）；不包含Bash/文件工具/web_publish。已结束任务保留30分钟，查询不续期；内存记录随任务管理器销毁而丢失，未显示的记录仍可凭ID查询。

任务description由后端在创建时固定生成，最多120字符；act概括前两步及剩余数量，覆盖全部16种动作及其模式。使用ref/page ID等确定目标，不额外观察或猜测业务意图；坐标动作优先保留起终点坐标，单步使用完整摘要预算，多步超限时用省略号截短标识。不复述填入/桌面输入内容、文件正文或密码；URL去除凭据、查询串和片段，data等内联内容仅报告协议。状态由state表示，摘要不声称执行成功。

所有网页图片统一为**当前已渲染文档从顶部到底部的整页图**，不需要 full_page 参数。`browser_snapshot(image=true)` 附带带 ref 标注的整页图；`screen_observe(view="page")` 返回无标注整页图，可定时连续捕获或保存原图。两者共用捕获、文档校验和坐标映射。完整文档包含 iframe 在父页面上实际呈现的区域；iframe 内部独立滚动的全部文档、尚未加载的无限列表不等于父页面整页范围。语义观察和读取仍进入同源、嵌套和跨域 iframe，不受截图范围限制。

观察将控件、独立文字、语义区域及真实浮层合成一棵精简树。行包含可操作 ref、ARIA role、名称和状态；缩进保留区域、列表项、对话框和 iframe 等必要归属。普通无语义包装压缩，同源且已被控件名称表达的文字不重复；不同区域相同的文字仍保留。长正文通过 browser_read 读取，不混入页面结构。面板依据明确语义、原生 dialog/popover 或展开控件的 aria-controls 识别，不仅凭 fixed/absolute 定位认定浮层；不会猜测触发控件或业务价值。普通观察不返回独立 layers、CSS 坐标、position 或 z_index。

browser_read 的 query 返回匹配元素的 ref、role、name 及有界文字；context 仅来自实际观察到的具名语义祖先，保留最近三层，最多300字符，省略无上下文的字段。裁剪时附 context_truncated=true；同名结果保持多个候选，不猜测目标；空结果保留 matches=[]。

view=tabs 返回每个页面的 page_id；动作触发的弹出页也在 popups 报告。浏览器原生窗口和资料选择器以 browser_ui/health 标明，使用桌面截图和桌面动作。已弹出的 alert/confirm/prompt 可能阻塞渲染器，snapshot 返回 page_blocked，保留页面身份和健康信息；只读观察不自动关闭对话框。

browser_read 的 query 在所选页面及其 iframe 的已观察元素名称或文本中查找不区分大小写的子串，最多20项；可用 ref 限定子树，与 extract 同传仍报错。query/extract 使用各自的结果上限，忽略 max_chars；table 最多5表、每表100数据行、每行40格；links 最多200项；fields 最多150项、单值最多1000字符，保留控件原值（含换行和 select 的 option value），字段 ref 立即可操作；较长控件值可用 ref 正文读取。正文默认主内容并包含各 frame，ref 则限于该子树。内容、单元格或集合达到上限时均标记 truncated。

### 有序动作与任务控制

```json
{"actions":[{"do":"fill","ref":"eabcdefghij","text":"query"},{"do":"click","ref":"eklmnopqrst"}]}
```

整批参数在执行前校验；运行时遇错停止，已发生的动作不回滚。默认 then=diff，可选 snapshot；整批结束后观察一次。失败时尽力附当前诊断快照。观察失败另报 observation_error，不伪装成空差异，也不改变已成功动作的任务状态。超时/取消保留已产生的部分结果，不额外启动昂贵观察。

公开 MCP 结果没有 value 包装。state/task_id 表达任务状态与身份；completed_steps/total_steps 表达已完成和总步骤数；page 包含 page_id/url/title，未涉及页面时不补造。失败保留 error、failed_step（step_number 从1开始）、side_effects、已完成数量和已有 files/popups。失败步骤即使已经产生部分副作用也不计入 completed_steps。任务成功不代表网站业务成功；不自动重试或生成业务结论。内部 worker 的 value/checkpoint 封装仍由原任务所有者管理。

observation 是有界语义快照，observation_diff 是相对于该 MCP 会话此前返回观察的多行差异，两者互斥。差异使用 + 新行、- 旧行、无标记上下文；同 ref 的前后行表示描述变化，消失行保留旧名称。它是观察集合变化，不证明 DOM 删除，也不证明变化由本批动作导致。没有变化为 observation_diff=""；未观察则两字段均省略。

```diff
  region "文件上传"
-   eabcdefghij button "添加文件" [expanded=false]
+   eabcdefghij button "添加文件" [expanded]
+   menu
+     eklmnopqrst button "从计算机上传"
```

基准属于 MCP 连接而非网页或共享空间，按页面、文档和观察模式匹配；只接受该连接生成且已提供的观察。跨连接查询任务时，保存的差异转换为其快照，不引入对方基准。read、截图和子树观察不推进基准；任务查询返回已保存结果，不重新采集，首次取回本连接生成的观察可以登记基准，旧结果重复取回不会倒退基准。无可用基准、导航、模式变化、重排/移位、并发结果的基准已过期或不可靠/过大的差异返回 observation。连接最多缓存8页，重连或缓存淘汰后的首次观察返回快照；这是有界会话缓存，不写网页或持久工作区，也不增加任务服务。

语义输出最多150行、24000个字符，每项内容最多300 Unicode字符；截断明确返回 observation_truncated=true，未采集内容不判定为移除，不完整基准不能生成确定的增量。独立扫描失败返回 observation_error 或观察任务错误，与截断区分。页面截图和文件产物预算沿用原合同。

| 动作 | 参数 |
|---|---|
| click | ref；或 image_id/x/y；可选 button=left/right/middle、double=false |
| hover | ref |
| fill / select / check | ref；分别 text 和 submit=false、value、checked=true |
| press | key；target=page（默认）可选 ref；target=desktop 操作桌面焦点，忽略 ref。统一使用 Enter、Escape、Control+a 等键名 |
| drag | ref/to_ref；或 image_id/x/y/to_x/to_y；网页图两端须可放入同一视口 |
| navigate | mode=open（默认）要求 url；mode=back/forward/reload 忽略 url |
| tab | action=new/close/focus；new 可选 url、忽略 page_id；close/focus 可选 page_id、忽略 url |
| scroll | target=page（默认）：ref，或 until_text（exact=false、max=15、dy=800），或 dy=800 CSS像素；target=desktop：image_id/x/y/delta，delta=-30..30轮步，正数向下 |
| wait | text/gone/secs 恰选一个；固定 secs 为 0..60 秒；条件等待由后端限制，满足条件立即返回 |
| upload / download | ref；上传 paths 1..20 个空间内绝对文件路径；下载事件等待和文件保存共享后端60秒预算 |
| dialog | accept 必填、text 可选；本批后续网页步骤的 JS 对话框策略，须放在网页步骤前；默认关闭且报告 |
| desktop_type | text；向桌面当前焦点输入文字 |
| window | mode=normal/maximized/fullscreen |

upload 先以空间命令/文件身份验证可读普通文件，再操作 input 或隐藏文件选择按钮；download 等待并保存真实下载，返回 files 元数据，不自动预览。已保存下载在后续失败、超时或取消时仍保留。桌面文字、按键和窗口动作不需要 Playwright 附着，个人资料选择器、原生选择器或无响应页面仍可恢复；不自动重置、导航或关闭无关页面。

指针坐标统一使用返回图片的像素，image_id 绑定捕获的页面或桌面；网页坐标动作还要求选定页面与截图页面一致。网页整页图自动映射到文档坐标，点击页面底部时由动作显式滚动到目标再点击；拖动两端须可放入同一视口。图像有效期60秒，并校验页面/文档、URL、视口、滚动位置及整页尺寸；桌面图绑定捕获前后一致的前台窗口。过期、导航、窗口变化或尺寸变化返回 STALE_OBSERVATION。结构元素优先使用 ref，图像适合 canvas、图形和原生窗口。

所有浏览器/截图任务均用同一入口查询或取消，不启动新修改任务：

```json
{"op":"wait","task_id":"...","wait_secs":30}
```

op=cancel 请求取消；wait/cancel 使用 task_id、wait_secs（默认30，0..30）。0 立即读取，轮询不续期。bash 任务不在此范围。

任务状态查询短暂失败时，只继续查询同一任务，不据此取消或重放动作；用户取消、权限失效和原始任务期限仍生效，持续失联仍按既有有界监管与控制阻断处理。

面向 Agent 的可配置时间统一使用整数秒：固定等待 `{"do":"wait","secs":2}`，条件等待 `{"do":"wait","text":"完成"}` 或 `{"do":"wait","gone":"加载中"}`。条件满足即返回；`secs=0` 不暂停。纯延时不选择页面、不附着浏览器，也不返回页面观察；仍遵循控制权、取消及原任务期限。整批动作任务后端上限120秒，包含准备、全部动作和批末观察；普通交互上限30秒，导航、新页打开URL、条件等待和下载上限60秒，页面观察（包含helper前置观察）上限30秒。一个动作内的子操作共享期限，所有阶段受任务剩余时间约束；固定延时仍受整批期限限制。公开 schema 不再提供顶层 timeout_secs/action_timeout_secs 或 wait/download 步骤 timeout_secs；旧调用携带这些字段时静默忽略。首次调用最多等待 30 秒，未完成返回 task_id，后续 `op=wait` 默认最多等待 30 秒；这两个返回窗口不截断页面等待。单次通信、取消和清理仍保留有界保护；任务查询不续期。内部计时保留毫秒精度；公开帧时间统一为 `elapsed_secs`，不是可配置参数。

参数先按操作归一化，再校验有效字段，避免模型随手带入默认值导致无效重试：execute 忽略 wait_secs；wait/cancel 忽略 page_id、then，并把空 actions 数组视为未提供。非空 actions 与 wait/cancel、非空 task_id 与 execute 仍属意图冲突，明确报错；不会猜测是继续任务还是执行新动作。步骤内已知但无关字段在类型和范围检查前移除：ref 定位忽略图像参数；ref 滚动忽略其他滚动参数；desktop 按键/滚动忽略网页参数。未知字段仍报错。顶层无关执行选项不做范围校验。顶层可选字符串选择项的空白值与 null 视为省略；fill.text 等实际内容保留空字符串及空白。

步骤 wait.text/gone 使用与观察一致的可见文字或可访问名称（输入框名称、按钮标签、图片 alt 等），不区分大小写、按字面子串匹配并包含 iframe；gone 在所有可观察匹配消失后完成，页面读取失败不代表消失。顶层 op=wait 仅查询任务，两种等待不混用。

默认快照、读取和动作均不附带全量页面列表；需要查找、聚焦或关闭页面时调用 `browser_snapshot({"view":"tabs"})`，该路径只查询 Chromium 页面清单，不读取 DOM 或探测各页焦点，并忽略 page_id/ref/image。弹窗直接返回 popups 中的新 page_id；达到页面上限时错误提示引导查询列表并复用或关闭旧页，不自动淘汰。正常 health、false 截断标记和非 iframe 的空 frame_path 省略；空差异以 observation_diff="" 表示；空查询结果 matches/fields 等保留，表示查询确实没有结果。异常健康状态、真实字段值、失败位置与副作用仍完整返回。

公开接口仅保留上述16种动作及顶层 op，不保留旧动作名或顶层 action 的兼容入口。

动作任务，以及其他仍在运行、失败、超时或取消的已接纳任务，在外层返回 duration_secs（经过时间，最多一位小数，含准备和执行；运行中为累计时间，终态固定）。仅动作执行、连续截图，以及仍在运行或执行失败的任务返回 started_at（UTC，格式 `MM-DD HH:mm:ssZ`，例如 `10-20 11:23:04Z`）；普通快照、页面列表、读取和单张截图成功时省略开始时间。成功的快照、读取和截图省略总耗时；连续截图保留每帧 elapsed_secs，单张截图成功时省略帧计时。Bash 不附加开始时间或耗时内容块，命令自身输出原样保留，超时错误仍保留必要诊断。op=wait/cancel 沿用原任务计时，重复查询不会重新起算或增加已完成耗时。task_id/state/control 保留用于查询、追踪和控制权判断；所有状态省略内部 phase 和 worker 原始日志；成功省略副作用记账，失败保留副作用与未确认停止状态。请求在参数校验或准入阶段被拒绝、没有创建任务时，不伪造任务开始时间。

优先用 browser_read(query=...) 找已知目标，用 ref 操作并利用默认差异反馈；需了解布局时 snapshot(image=true)，原生窗口用 screen_observe(view=desktop)，连续图片只在需要时间序列时启用 count。元素动作自带等待，等待业务完成优先 text/gone，纯延时才用 secs；state=running 时查询原 task_id，不重复提交。引导主要放在工具说明，运行结果仅在异常需要下一步操作时提示。普通观察、读取和下载结果中的源 URL 最多1000字符，页面标题最多200字符，超长内容带 truncated 标记；文件路径和保存内容不因此截断。错误保留稳定代码、简短原因与可执行的下一步；原始 worker 日志留在内部，已完成步骤、副作用及产物不丢弃。观察子进程超时返回 PAGE_UNRESPONSIVE，其他启动或执行失败返回 BROWSER_UNAVAILABLE；底层数字退出码不作为工具错误码。错误提示引导重新观察或检查桌面，不自动重放可能已有副作用的动作。

STALE_REF 后根据当前观察更新 ref，从未完成步骤继续，不重放已完成步骤。HUMAN_CONTROL 时可继续观察，等用户交还控制权再操作。任务 succeeded 只表示工具动作执行完成；网站提交是否成功须看页面反馈，不能据此重复提交。

### 截图与连续观察

```json
{"view":"page"}
```

page 捕获当前顶层文档完整布局，独立滚动容器和 iframe 只显示其当前渲染区域，不展开虚拟列表、无限列表或懒加载内容；desktop 捕获整个桌面及原生窗口，忽略 page_id。截图不主动滚动、不聚焦，捕获后核对文档、滚动位置和视口几何未改变；不用 Playwright 附着。目标只在首次选择，同一标签可导航；关闭后失败并保留已完成帧，不替换目标。桌面截图只依赖显示服务，浏览器卡住时仍可使用，不为截图重启已有浏览器。

第一帧准备好即捕获，后续从捕获开始计至少 interval_secs 秒（默认 1，count=1 时忽略），处理慢则顺延；0 连续捕获。最多8帧，含准备、处理、等待的总期限最多600秒。运行中返回进度，终态含 view/requested_count/captured_count/frames，frames 有 index/image_id/width/height/format，width、height 为返回图片的像素宽高；仅缩小时另含 original_width/original_height；连续截图或未成功结束时另含 elapsed_secs，单张成功截图省略帧计时。图像按同顺序放在标准 MCP image 块，不复制进文本。失败、取消、超时保留已有帧。

截图先取得完整范围的原始 PNG，不提前缩放或裁掉底部；不主动滚动以扩展懒加载或无限列表。源 PNG≤10 MiB、解码画布≤1600万像素；不能在源预算内表示完整范围时明确报错。

图片返回使用共享编码管线：`screen_observe(view=desktop)` 保留桌面原始像素宽高，不缩放；`browser_snapshot(image=true)`、`screen_observe(view=page)` 和 `read(multimodal=true)` 只进行一次 CatmullRom 等比例缩小，最长边≤2560像素，小图不放大。随后统一编码为 WebP Q90（method=4），显式固定 alpha_quality=100、alpha_compression=1，透明通道无损编码。快照标注在原始捕获阶段完成。图片元数据返回 width、height、format，缩小时增加 original_width、original_height（源图片尺寸）；保留所需 image_id、帧关联或文件路径。桌面不返回原始尺寸；编码压缩本身不算缩放。方向修正和首帧标记留在内部。工具说明只说明桌面保留像素尺寸，不披露编码实现。坐标动作使用返回图片的像素坐标和 image_id；整页图片在内部按实际宽高分别换算横纵坐标，避免取整后的长页纵向偏移。`browser_act(wait/cancel)` 原样转交任务图片及尺寸，不再次压缩。

每次结果的图片编码字节合计≤8 MiB，连续截图按实际累计大小计算预算，不按张数降低质量或分辨率；预算不足报 IMAGE_BUDGET_EXCEEDED 并保留已完成帧。完整结果仍遵循12 MiB传输预算，不进行逐级降质或二次缩放。长页面缩小后文字可能变小，需要精读时使用 browser_read 或查看保存的原始 PNG。

默认不落盘。save_original=true 按缩放和 WebP 编码前的完整原始 PNG 字节保存，可选 save_dir 指定绝对目录，默认 `/workspace/agent-space/artifacts`。缺失目录自动创建，沿用空间文件权限；save_original=false 或 null 时忽略 save_dir。文件名 `screen-<10位Base62>.png` 原子创建、不覆盖。files 含 frame_index/image_id/path/format，不重复图片尺寸或文件体积；可经工作台、DAV 下载，或 read(multimodal=true) 查看。保存失败保留当前可显示帧，返回 SCREEN_SAVE_FAILED。

所有工具自有返回中的文件体积，需要保留时统一为整数 size_bytes，不附重复的人类可读体积：浏览器下载的所有文件类型在 files/downloads 中均如此；JPEG、PNG、WebP、GIF 另用 format 表示格式，不重复文件名和 MIME 字段。write 成功返回 `{"path":"/workspace/example.txt","size_bytes":6}`，按实际 UTF-8 字节计算。图片展示元数据和截图原件保存记录不返回体积。下载文件保留原字节，后续 read 查看时才进入图片处理管线。网页字段、文件正文及 Bash 原始输出不受此命名规则改写；工作台和内部 RPC 的文件传输协议不因此改变。

### 页面数量上限

Server、Standalone 共用 `[browser].max_pages`，省略默认4，只接受正整数；重启服务后生效。每空间的共享浏览器分别计数，所有连接、窗口中的 Page 共用上限：tab、新窗口页面、网页弹窗和空白页均计入，导航、刷新、切换或移动已有页不增加数量；iframe、worker、alert 和文件选择对话框不计入。

`tab/new`由 desktop helper 串行准入；网页、桌面快捷键或人工新增的超额页面，在创建事件中关闭新页，不淘汰旧页，因此后者存在短暂创建窗口。只有确认页面关闭才释放数量，任务结束、超时或取消不释放仍存在的页面。页面守卫随 Chromium 启停，CDP 重连保留已接纳页面身份，监听不刷新桌面活动；浏览器重开时从实际恢复页面建立基线，包括经个人资料选择器延后恢复的初始页面。配置调小或恢复后已有页面超额时保留已有工作，阻止继续新增，允许复用与关闭。

满额只阻止新增，观察、截图、已有页导航和关闭、任务查询继续可用。自动化新增失败沿用 `error:{code,message}`，code 为 `PAGE_LIMIT_REACHED`，message 含实际配置上限及复用/关闭提示；保留已有任务状态、输出和副作用说明，不增加用量字段或改变正常工具结果。浏览器动作执行期间新增弹窗被拒绝时，中止该次动作任务并返回同一错误，已登记输出保留。没有运行浏览器动作时，人工或后台新增只关闭超额新页并记录日志，不给后续无关观察附加错误。

工作台的“显示控制”提供“打开浏览器”，调用已有认证 `/viewer` 入口：人工关闭 Chromium 后可重新打开同一个受管浏览器；已有页面时只复用，没有页面且无个人资料选择器时经同一数量准入补建空白页，不创建第二个浏览器实例，不改变控制权或续期人工输入权限。页面仍遵循上述上限；桌面自身休眠时继续使用原有“唤醒桌面”。

### 个人资料选择与休眠恢复

Chrome 个人资料（profile）与 Google 网站中同时登录的多个账号不同。[Google 官方说明](https://support.google.com/chrome/answer/2364824?hl=en)中的“启动时显示”控制个人资料选择器；[网站账号切换](https://support.google.com/accounts/answer/1721977?hl=en)是普通网页行为，页面及登录弹窗按 Page 上限计数。多个资料中实际打开的 Page 也共用每空间上限，不能按账号增加额度。

首次启动、人工明确关闭后重新打开，按 Chromium 的个人资料设置启动：启用选择器时保留选择界面，关闭时沿用浏览器的最后使用记录。选择器是 `browser_ui`，不计入 Page，不补建空白页绕过它，也不自动选择或修改账号。正文读取或网页操作需要选择资料时，沿用既有错误结构返回 `PROFILE_SELECTION_REQUIRED`；桌面截图和 `desktop` 操作仍可用，完成选择后重新观察即可。

300秒闲置休眠后唤醒属于原会话恢复：继续最后选择的资料并恢复原有资料中的页面，不再次要求选择。仍由现有 desktop helper 负责闲置、关闭和启动；根据同一 Page owner 已记录的页面状态判断是否需要恢复，闲置回收不依赖 CDP/DOM 响应，浏览器失去响应也沿用原有有界关闭流程。受管休眠确认停止 Chromium 后，以空间执行身份在持久 Local State 中写入原生 `was.restarted` 会话标记，因此 helper/VM 继续按原策略回收后仍可恢复。仅检测到该标记时，启动增加 `--hide-crash-restore-bubble`，避免强制回收留下的 Crashed 状态阻断原生会话恢复。浏览器自行消费并清除该标记，不改 `show_picker_on_startup` 或 `last_used`，不另存资料选择状态。启动后尚未打开资料页面就休眠时，仍保留选择流程。此行为依赖镜像固定的 Chromium，升级须复验选择器和多资料恢复；[Chromium 启动实现](https://github.com/chromium/chromium/blob/971a7443b0c9b0a9b2860529b33331b76077ec62/chrome/browser/ui/startup/startup_browser_creator.cc)与[偏好定义](https://github.com/chromium/chromium/blob/971a7443b0c9b0a9b2860529b33331b76077ec62/chrome/common/pref_names.h)为依据。

### 任务、错误与输出预算

期限从 Host 接受起计，覆盖准备和执行；等待不延长期限。guest 监督 Node worker，超时或取消时按进程身份先 TERM 后 KILL 并核对后代；HTTP 等待者断开不释放实际工作所有权，也不重放动作。修改清理确认后才能移交控制权，无法确认时进入 BLOCKED，禁止新修改。

运行中返回 task_id/state、剩余时间及上述计时字段；终态加非空的 value/files/output_warning 或截图帧说明，图片单独放 MCP image 块，不重复进文字或 structuredContent。phase 和 worker 原始日志只供内部诊断，不进入 Agent 返回。成功省略空字段及内部清理标记；失败使用 `error:{code,message}`，任务执行失败有 `isError=true`，修改任务保留有意义的 side_effects，BUSY 附 active_task_id。参数错误使用协议 InvalidParams。未知错误保留有界原始原因，去除控制序列后最多2048字符；定位器错误保留目标和主因，不重复返回重试日志。

| 输出 | 限制 |
|---|---|
| 工具结果传输 | 每次12 MiB，计入 Base64、JSON 和协议封装预留；超限明确报错，不截断图片 |
| snapshot | 元素候选≤600、输出≤150行、正文≤40行、浮层≤40、tabs≤50；显式 truncated |
| 动作下载 | ≤16文件，只回元数据，已保存输出以原子检查点保留 |
| 内部 worker stdout/stderr | 各≤32 KiB / 500行，有截断标记；不直接返回 Agent |
| 截图坐标缓存 | 最近≤32张，生命周期从实际捕获起计 |
| 任务缓存 | 完成后最多30分钟，Host 重启失效；网站副作用不回滚 |

browser_act(op=cancel) 是取消请求，wait_secs 对 wait/cancel 均适用；本次等待结束后仍可能返回运行中状态，按公开 state 字段判断：running 表示尚未结束，succeeded/failed/timed_out/cancelled 为终态。TASK_NOT_FOUND、超时、断连和失败不能证明网站尚未收到请求，应观察当前状态和产物后再决定下一步。bash 不纳入浏览器任务查询。接入方须允许至少12 MiB的工具结果与 SSE 数据帧；外部 Agent、模型或网关是否完整接收图片需独立联调，本仓库的验证不代表外部系统已经兼容。

## read 的图片读取

read 保留 path/offset/limit，新增 `multimodal`，默认 false；自定义非桌面空间同样支持，不新增文件查看工具。

| 输入 | 行为 |
|---|---|
| 文本文件，任意 multimodal | 保持行号、offset/limit、默认500行及现有文本截断，不施加图片源大小限制 |
| 图片，默认关闭 | MULTIMODAL_REQUIRED，提示设置 multimodal=true |
| 支持图片，multimodal=true | path、width、height、format，缩小时另含 original_width、original_height，加一个标准 MCP image 块 |
| 图片同时指定 offset/limit | 忽略行范围；文本读取仍校验正整数行范围 |
| 不支持、损坏或超限媒体 | 具体执行错误，isError=true；不退回文本读取 |

按内容识别 JPEG、PNG、WebP、GIF，扩展名不决定处理方式；BM、II* 等短 ASCII 前缀不单独作为媒体依据。GIF、动态 WebP、APNG 只取首个合成帧；不调用额外模型做 OCR 或图片描述。源文件≤10 MiB，解码画布≤1600万像素，每次一张。完整解码前检查尺寸、像素与解码预算，修正 EXIF 方向并按前述统一规则返回最长边≤2560像素的 WebP Q90，保留透明度；不会修改源文件。返回元数据含 path、width、height、format，缩小时增加 original_width、original_height。保存的原始 PNG 经 read 查看时同样返回处理后的图片，下载原文件仍保留原始像素和 PNG 字节。

文件脚本复用真实路径、同一打开句柄、共享读锁和有界二进制读取；沿用原文件准入、四路队列、期限、取消及活动租约。内部图片报文有独立预算，包含 Base64/JSON；50 KiB 默认文本上限不变，媒体报文截断、缺失或解析失败直接报错。Rust 共享模块负责识别、校验、方向、缩放和编码，无需 Workspace 安装 Pillow。每个处理进程的图片管线最多并行两项，等待及处理计入请求期限；取消或超时不会提前释放仍在执行的同步解码/编码所占许可及活动租约，实际处理结束后回收。Server、Standalone 共用实现。

## 控制权及生命周期

默认 AI；多人可同时观看，只有一个桌面页面会话可接管。服务端按完整 RFB 消息过滤键盘、鼠标、剪贴板及屏幕调整输入；其他观看者仍接收图像。接管只取消当前浏览器自动化任务并等待清理；bash/read/write/edit及工作台文件传输不参与桌面控制门禁，人工接管时仍可执行。此控制权是浏览器工具与VNC输入的协调规则，不是对任意bash/用户进程操作桌面的安全隔离。只有接受的输入续期60秒，轮询、ping、被拒输入和观看不续期。交还、持有人连接断开或无动作到期后回 AI，并释放已按下的键和鼠标按钮。

浏览器工具、显式唤醒、成功人工接管和实际人工输入刷新桌面活动；接管成功时从该时刻重新计算桌面空闲，同时刷新空间工作活动，60秒无输入交还AI的规则不变。接管失败、主动交还、任务轮询、状态/健康查询和持续被动观看不刷新。无活动300秒且无任务时关闭桌面，profile/工作文件保留；后来调用工具或点击唤醒重新打开。启动不传 about:blank URL，优雅 Browser.close 保留标签。VM 空闲回收仍归原 SessionManager，不增加另一套 VM 生命周期。

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

本机文件先通过工作台上传，默认落在 `/workspace/<文件名>`；Agent 已生成的文件直接使用空间内绝对路径。网站上传使用 `browser_act` 的 upload 动作，提供文件控件或上传按钮的 ref 和 paths。原生上传对话框已经打开时，先 `screen_observe(view="desktop")`，再用 press（target="desktop"、key="Control+l"）、desktop_type（text=绝对路径），基于新桌面截图用 click 点击“打开”；关闭对话框后重新观察页面。以网站实际接收的字节或成功状态确认完成，不能只凭文件名已显示判断。

自动化下载使用 `browser_act` 的 download 动作和下载按钮 ref，或收取其他动作触发的下载；工具保存到 `/workspace/agent-space/artifacts` 并返回 files。人工桌面触发的普通下载使用 Chromium 的下载目录，默认也是该产物目录；自选路径继续保留。浏览器的临时下载目录与浏览器进程使用同一空间身份，避免下载被临时目录权限拒绝。

文件面板不列 profile，但空间拥有者通过原文件工具有权读工作区。Web发布只允许可信内容；同源页面不能依赖 Cookie Path 实现脚本隔离。外网TLS和网关由部署方负责，入口要求见[部署指南](deployment.md)。

## 验证入口

Rust 测试与原真实 VM 回归见 AGENTS.md。新增独立资格测试需要测试 Host、测试凭据文件和已加载镜像：

```bash
export AGENT_SPACE_BROWSER_TEST_DIR=/path/to/independent-qualification
python3 tests/test_bash_vm.py
python3 tests/test_browser_vm.py
python3 tests/test_browser_pages_vm.py # Page上限、并发、弹窗、新窗口、取消
python3 tests/test_browser_profiles_vm.py # 选择器、跨资料上限、真实300秒休眠恢复
python3 tests/test_desktop_files_vm.py
python3 tests/test_browser_lifecycle_vm.py # 真实300秒，串行且勿操作该空间
MSB_HOME=/path/to/isolated-msb python3 tests/test_migration_cli.py
AGENT_SPACE_BROWSER_TEST_MSB_HOME=/path/to/isolated-msb python3 tests/test_failed_probe.py
python3 tests/test_exec_owner.py
cd desktop-ui && npm ci && npm run build && npm test
```

同一空间的浏览器脚本与人工输入按控制权互斥；基础文件工具、bash和工作台文件流允许与桌面操作同时进行。破坏性生命周期测试仍须使用独立空间，不能与其他测试争用VM启停。

这些测试明确区分脚本 MCP、浏览器/UI、真实 VM、Docker候选、实际模型Agent和第三方网站。fixture通过不能代替Grok登录/付费生成成功；实测和未验证边界必须登记，不自动切换现有服务。
