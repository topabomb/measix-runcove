# Measix Runcove 架构

Runcove 在一个仓库中维护共享核心，提供托管多空间版（Server + Workspace）和独立单空间版（Standalone）。Server 使用 MSB VM；Standalone 直接管理本容器的一个空间，不含 MSB、管理 API 或多空间登记库。Workspace 是 VM 使用的空间 rootfs，自身不带 MSB。入口、编排和构建分别维护，共同任务、文件、浏览器及工作台合同保持一致。本页先说明共享边界，后续账号、VM 和管理 API 章节只适用于 Server；开发与验收入口见[维护指南](development.md)。

独立入口位于 `agent-space-mcp/src/bin/runcove-standalone.rs`，以无默认 feature 构建；`ProcessEngine` 通过同一 `AgentHandle` 边界执行本地前台 owner、受管 helper 和回环 TCP。MCP handler、DesktopHub、任务/控制权、RFB、文件、DAV、发布和工作台保持共用；不连接 MSB，不调用创建/删除 VM 或卷，也不启动 VM 空闲回收循环。

Standalone 的 `StateStore::standalone` 只建立固定空间的只读内存访问身份，不创建 state.json 或用户表。配置只保留入口、凭据环境引用、执行、日志、可选 DAV 和发布设置；外部 DAV 默认关闭。容器 `/workspace` 持久化文件、profile、产物和既有暂存归属，Cookie、任务和临时发布随服务结束。

Standalone 的桌面控制 owner 异常死亡时不能证明所有独立桌面后代已经结束：保留受保护的实例记录，控制进入 `BLOCKED`，拒绝创建第二个桌面 owner，错误明确要求重启容器。基础文件和命令仍可使用；容器重启由 Docker 清空原 PID namespace 后恢复。正常关闭桌面和正常停服继续使用既有后代清理边界，不新增通用守护或第二套回收器。

Server 是单节点、可独立部署的托管空间服务。它为每个本地账号维护一个隔离的 microsandbox VM、持久根盘和 `/workspace` 独立磁盘卷，提供 Streamable HTTP MCP、文件状态用户管理，WebDAV 文件交换，以及随空间运行 session 结束的临时网页发布。当前锁定 microsandbox SDK、CLI 和运行时 0.7.2，使用可核验的本地临时补丁；VM 元数据由 SDK／runtime 维护，Agent Space 不直接写 MSB 数据库。

本文件说明实现的所有权与生命周期；接口、配置、操作和 MEASIX 接入步骤见[功能与接入参考](server-api.md)，容器部署见[Docker 部署](deployment.md)。WebDAV helper 和外部 DAV 入口已实现，固定版本验证范围单独登记；当前不提供平台 quota 或 compute/storage metering。

`[workspace].image` 接受自定义字符串或桌面预设对象。schema3 为每个空间固定镜像选择；自定义类型提供原五工具，桌面预设自动增加三个浏览器工具、screen_observe 和 `/u/{username}/desktop/` 工作台。默认值改变不替换已有根盘或能力；详见[浏览器与云桌面合同](browser-desktop.md)。

## 执行身份与环境管理

两种编排共用工具能力和调用语义，执行身份由各自的空间隔离边界确定：

| 执行对象 | Server | Standalone |
|---|---|---|
| Runcove 核心服务 | 位于用户 VM 外的 Host 服务 | 本容器内的 root 服务 |
| 受管 helper 控制进程 | VM 内 root | 容器内 root |
| bash、文件工具、browser_run 用户脚本、原图保存及产物枚举 | VM 内 root | UID/GID1001 |
| Chromium、桌面应用和内部观察进程 | VM 内 UID/GID1001 | 容器内 UID/GID1001 |

Standalone 保持一个使用者、一个空间、一个容器。Agent 可以管理工作文件、用户应用和项目依赖；Python 虚拟环境、项目内 npm 依赖等使用可写目录。系统依赖由部署维护者在镜像构建时提供，Agent 没有运行时 root 或任意 sudo 权限。root 控制进程负责服务入口、凭据和运行记录，UID 隔离用于防止普通用户权限下的任务直接读写控制文件或终止控制进程；资源耗尽和程序故障仍由资源限额、任务期限及进程清理处理。

Standalone 的 root 控制进程不把访问凭据传给 payload。桌面 profile 准备和产物枚举在降权子进程内完成，包括对用户可修改的父路径进行遍历，最终任务结果与观察保存在现有 Job 内存中，读取 worker 结果使用不跟随链接的固定文件句柄、归属和大小校验。DAV/static-http 的 root 身份父进程管理 UID1001 数据子进程；父死强杀子进程，正常停服仍先 TERM 后有界收敛。

Server 的 Agent 可以在 VM 内使用 root 管理系统依赖。Host 核心服务由 VM 边界隔离；guest root 仍能修改 VM 内的 helper 和桌面文件，受管进程约定用于协作和防误操作。新增工具的文件访问沿用所在空间的命令/文件身份，避免对同一路径产生额外的权限差异。

## 1. 组件和权威状态

| 组件 | 责任 |
|---|---|
| `agent-space/src/state.rs`（`agent-engine` crate）的 `StateStore` | 用户、凭证摘要、空间 ID 和精确资源清单的唯一持久权威 |
| `agent-space/src/workspace_service.rs` 的 `WorkspaceService` | 协调管理 API 的用户状态变更、VM 停止、删除和重试 |
| `agent-space/src/resources.rs` 的 `ResourceReader` | 管理 GET 的资源摘要、短时缓存、有界查询和独立指标失败；不拥有 VM 生命周期 |
| `agent-space/src/session.rs` 的 `SessionManager` | 每空间唯一运行所有者；队列、容量、活动、预览配置和 helper 生命周期 |
| `agent-space/src/engine/msb.rs` | microsandbox VM、卷、guest TCP 和受管 helper 的底层操作 |
| `agent-space/src/config.rs` | 运行 profile、生命周期、外部 origin、状态目录和管理员凭证来源；拒绝动态模式并行使用静态用户 |
| `agent-space/agent-space-mcp/src/mcp_auth.rs`、`server.rs` | 每个 MCP HTTP 请求与 session 鉴权；按镜像提供五/九工具和执行前准入复核 |
| `agent-space/agent-space-mcp/src/admin_api.rs`、`web_publish.rs`、`webdav.rs`、`origins.rs` | 管理接口；发布/DAV 认证及流式反代；各入口 authority 与浏览器来源边界 |
| `agent-space/agent-space-mcp/src/main.rs` | 路由装配、显式状态初始化和静态用户导入、启动接管及清理重试 |
| `agent-space/static-http/src/main.rs`（`agent-space-static-http`） | 运行在 guest 内、只读服务指定文件或目录的固定版本程序 |
| `agent-space/webdav`（`agent-space-webdav`） | guest 内受限文件系统、DAV 协议、锁与原子 PUT |
| `agent-space/desktop-helper`、`guest/desktop` | 固定受管桌面进程与匹配 Playwright worker，唯一可见浏览器 |
| `agent-space/desktop-ui`、MCP `desktop.rs/rfb.rs` | 中英文工作台、空间 Cookie/文件流及 VNC 输入门禁 |
| `agent-space/docker/` | 镜像、运行时装载、真实 KVM 验证及离线包 |

DAV 凭证可由服务端自动生成或由管理调用方指定；同值重试保持 keyId，不触发状态写入和在途请求取消。自定义 token 仍只保存摘要，认证和撤销由 Host StateStore 负责，guest 不持有凭证。输入与重试规则见[接入参考](server-api.md#dav-凭证首次签发与轮换)。

管理 API 经 WorkspaceService 修改状态。Server 的 MCP、DAV 和发布入口读取同一个 StateStore，再向同一个 SessionManager 准入；它们不保存第二份用户或 VM 状态。没有独立的发布回收器或 URL TTL、宿主端口池、Hub 身份映射表或第二套空间回收器。

容器异常退出后，已有 `crashed` VM 通过正常工具或 DAV 请求恢复原根盘与工作卷。Linux PID 可能在新容器中被无关进程或线程复用，数字 PID 存活不能证明旧 VM 存活。MSB SDK／runtime 的临时补丁通过 pidfd 固定观察对象，以继承 fd 99 与预期 lifecycle lock 的 dev／ino 核对所有权；明确的其他程序、其他空间或线程 ID 可证明 PID 已复用，身份不确定则报错。状态收敛沿 SDK／runtime 原有锁、重读和更新流程完成，保留历史 run.pid，不改写已有终止原因或补造退出诊断；Agent Space 不直接写 MSB 表。恢复保留空间身份和原盘，不向复用 PID 的无关进程发送信号。补丁边界及升级撤销条件见[运行时维护](runtime.md#linux-运行身份)。

单用户管理 GET 通过 SessionManager 提供的引擎及默认 profile，只读检查 SDK metadata；运行时可采集客户机内存及独立工作卷 `fstatvfs`。该通道不创建 session、不走执行队列、激活或活动租约，也不持有运行锁。状态每次重新查询，观测缓存 5 秒；暂停/停止只返回可用历史磁盘，内存不伪装为零。并发上限为 4，分阶段超时为 3 秒；空间及运行身份在采集后复核。缓存非持久、按空间 ID 隔离、删除清除，服务重启后可暂无数据。没有 CPU 使用率、常驻采集器或时序存储；字段、来源及错误语义见[资源摘要](server-api.md#资源摘要)。

受管 helper 的分发、实例身份、配置、日志和清理统一见[受管进程](#受管进程)。Host 持有运行句柄；helper 只管理自己的实例及后代，不改写用户 AGENTS.md。

WebDAV helper、独立凭证、管理 API、Host 流式代理和 DAV 专用运行槽共享现有空间所有者。文件语义与边界见[WebDAV 参考](webdav.md)，固定版本验证与部署身份见[验证记录](validation-records.md)。

## 2. 用户、凭证和对外路径

本地 `username` 唯一、不可改名，格式为 `^[a-z0-9][a-z0-9_.-]{0,63}$`。一个账号直接持有一个 `agentSpaceId=spc_<uuid>`；VM 停止或服务重启不改变它。删除后重建同名账号得到新空间 ID、新磁盘和新凭证。用户名只是本服务账号键，不能单凭路径证明身份。

| 对外路径 | 准入规则 |
|---|---|
| `/u/{username}/mcp` | 每个请求核对用户 Bearer、路径用户名、`agentSpaceId`、`keyId` 和 MCP session 绑定；排队工具执行前再次核对 |
| `/u/{username}/pub/{name}/*` | 核对账号 active、空间实例、当前发布和自动 Basic / X-Publish-Key 凭据 |
| `/u/{username}/dav/...` | DAV origin 上验证独立 Basic/Bearer，完整流期间监控当前账号、空间及 key |
| `/u/{username}/desktop/` 与 API/WS | 桌面预设；现有 MCP 身份换取有限 Cookie，nonce与同源保护，服务端输入门禁 |
| `/admin/v1/users...` | 服务级管理员 Bearer；普通用户 Token 无权调用 |
| `/healthz` | 服务级健康入口；动态状态模式检查状态文件和后端 |

`/u/{username}/` 指定 MCP/DAV 目标用户，凭证证明权限。Web 发布仅在完整发布前缀下转发，逐请求验证该发布自己的身份；前缀之外保留原有路由。WebDAV 有独立的入口和生命周期，不受 web_publish 控制。

MCP/admin 和 Web 发布共享主服务监听器及实际入口；server.public_base_url 留空时链接跟随请求，正式部署显式设置固定HTTPS基址。发布默认启用，可通过 [web_publish].enabled 关闭；DAV 保留独立监听器及origin，主服务和DAV路由不互通。无需发布子域名。发布使用 webNN、六位数字密码和独立 realm，无尾斜杠先 308 再 challenge。Cookie 限制 Path 不等于脚本隔离：不同发布共享浏览器 origin，只能发布可信内容。外部跨 origin 请求在代理前拒绝（显式用户顶层导航除外）。完整合同见 [Web 发布参考](web-publish.md)。

MCP 用户 Token 是随机 `keyId.secret`；状态文件只存 secret 的 SHA-256 摘要，每个用户最多一个有效 MCP Token；schema3 另外持有可选的独立 DAV credential，不能互相代用。创建或轮换时只返回一次明文；禁用、删除、轮换或同名重建后，旧 Token 及其 MCP session 立即失效。管理员 Bearer 来自部署环境变量，与用户和发布凭证分开。MCP 的无效凭证返回 401，有效凭证访问别人的路径返回 403；不存在或不匹配的发布入口返回 404。

MEASIX Core 接入时，约定将其不可复用的 `usr_<uuid>` **userId 原样用作本地 username**；Core 可改名的 `username` 不能作为空间键。这只是账号键约定，不表示 Agent Space 验证了平台身份。每用户凭据传递、跨服务生命周期和集成验收由调用方负责；具体接入合同见[参考文档](server-api.md#5-measix-control-hub-接入)。

## 3. 文件状态与管理生命周期

`state.json` 保存 `schemaVersion` 和用户记录：`username`、`agentSpaceId`、`status=active|disabled|deleting`、MCP `credential` 与独立 `davCredential` 摘要、`sandboxName`、`ownedVolumeNames`、固定 `image`、可重试的 `cleanup` 状态。资源归属在创建记录时固定，VM 可等到首次工具调用再创建。导入旧静态用户时保留实际 sandbox 和卷名；导入后状态文件成为唯一账号权威，不再并行读取 `[users]`。无 `[state].dir` 的旧静态模式仅用于尚未迁移的部署。

当前初始化生成 schema3。保留[已发布静态用户导入](operations.md#支持的静态发布基准)，并提供动态 schema2 的显式 dry-run/apply 迁移；旧用户均固定为custom，保留身份、凭据和磁盘，详见[运维手册](operations.md)。自动生成和自定义 DAV token 均保存完整摘要，使用独立 keyId 撤销；MCP 仍使用自己的 keyId.secret 认证。

同一状态目录只允许一个进程持有排他锁。写入先在内存中串行，再写临时文件、fsync、原子替换并 fsync 目录；提交成功后才向调用方报告。文件缺失、损坏、版本不支持或被另一个进程锁住时拒绝正常启动。新部署须显式 `init-state`；不会把丢失的旧状态文件误当成空账号库。替换后若持久性不确定，暂停管理写入和新运行准入，等待核对并重启。

管理状态转换如下：

```text
创建记录(active, VM 懒创建)
  ├─ 禁用 → disabled + 撤销 Token + 关闭预览 + 停止 VM（文件保留）
  │           └─ 停止完成 → 可恢复 active；须重新签发 Token
  └─ 删除 → deleting + 撤销 Token → 清理 VM/根盘/登记卷 → 删除记录
```

禁用停机失败保持 `stopPending=true`，服务启动和运行期重试；停机完成前恢复返回 409 `stop_pending`。删除返回 202 只表示意图已持久化，GET 变为 404 才表示清理完成。各写操作用请求中的 `agentSpaceId` 核对当前实例，旧管理请求不能触及同名重建的新空间。

禁用提交后先捕获并复核当前 Session 所有者，立即取消其执行和入口，再排队停止运行时。物理停机与删除共用清理门闩；取得门闩后重新读取空间身份、disabled 状态及待停机意图。周期任务的旧快照不能停止已经重新启用的空间，其他空间的慢清理也不能延迟本次执行取消。

## 4. MCP 执行与空间生命周期

```text
MCP HTTP → mcp_auth 每请求校验 → StateStore 当前身份
         → server.rs 工具与执行前复核 → SessionManager 空间执行/文件队列
         → ensure_running() → microsandbox → guest 执行
```

`bash` 使用串行队列，`read`、`write`、`edit` 使用下文独立有界文件队列。初始化、ping、`tools/list` 不启动 VM；`web_publish get/list` 不唤醒 VM。执行前再次检查账号状态、空间实例和 keyId，防止请求排队期间被禁用、轮换或删除。命令开始前释放 Runtime mutex，删除因而能有界取消在途命令；已发往 VM 的命令可能产生副作用，不会自动重放或回滚。

| VM 状态 | 容量占用 | 下次执行 |
|---|---:|---|
| 不存在、Created、Stopped、Crashed | 未运行时不占活动名额 | 创建或启动同一持久对象 |
| Starting、Running | 占用 | 等待收敛或复用 |
| Paused | 占用 | 显式恢复同一 VM 后连接 |
| Draining | 占用 | 等待停止完成，再启动同一对象 |
| 状态未知 | 保留名额 | 报错，不假定 VM 已消失 |

Server 激活在同一捕获 identity 和 60 秒总预算内显式调用 SDK `start_detached()`、`connect_with_timeout()` 或 `resume()`；Starting/Draining 等待沿用这份预算。状态读取失败不视为停止，命令连接不确定时不重放。异常退出后的明确磁盘锁冲突仅有界等待，不解锁或重建资源；细节见[WebDAV 准入](webdav.md#5-session准入与取消)。

一个空间只有一个运行所有者和 Runtime 门闩。激活、停止持门闩，用户命令执行期间释放；回收器仅在无 pending 工作、门闩可取得并再次确认超时后停止。`last_request` 跟踪 MCP 与成功准入的发布/DAV 请求，`last_work` 跟踪实际工具和文件工作；DAV 活动租约覆盖完整传输，释放时重新记录完成时间；认证失败、空闲 helper 和 get/list 不刷新活动时间；Web 发布的有限显式保活另由运行所有者检查。`server.mcp_session_timeout_secs=0` 允许同一 MCP 协议 session 在 VM 空闲停止后继续使用；协议 session、空间运行 session、VM 与磁盘不是同一生命周期。Paused VM 仍占 RAM；回收时先恢复以便优雅停止与文件系统同步。

VM 状态与传输连接健康分别检查。准入、执行或受管 helper 操作确认底层 Agent 连接已关闭时，运行所有者标记 reconnect；下次准入重新查询状态，Running 则重新附着同一 VM 并核验原 helper，状态未知则保留资源并报错。旧执行句柄的迟到错误不能使新句柄失效；端口拒绝、业务错误和普通超时不触发连接替换。恢复沿用此门闩，不另起回收器，也不自动重放已经提交的命令或 HTTP 写入。DAV 的具体错误和 TCP 合同见 [WebDAV 生命周期](webdav.md#5-session准入与取消)。

只有浏览器自动化参与桌面控制门禁；bash/read/write/edit与工作台文件传输不受人工接管限制。browser_run 任务由空间 DesktopHub 与 guest helper 监督，HTTP等待者不拥有任务生命周期；控制权、硬期限、300秒桌面空闲与原VM回收规则见[浏览器合同](browser-desktop.md)。bash输出在guest前台owner中有界消费，超时/取消保留部分输出并确认后代清理，正常退出不结束Agent显式启动的后台应用。

### 基础工具并发

每空间沿用一个VM运行所有者、同一启动/重连互斥和活动租约。bash保留串行队列；read/write/edit通过独立有界文件队列同时执行最多4项，超过上限等待，不因普通并发返回BUSY。启动、排队、同文件锁等待均计入文件工具的总期限；过期且未提交的请求不会迟到执行。调用方断开不释放仍在运行的执行所有权；账号禁用、删除或Host关闭向所有在途执行广播取消。无需改状态schema或用户配置。

固定短命脚本 `guest/file.py` 通过真实路径的63位摘要，在guest `/tmp/agent-space/file-operations.lock` 的一个字节范围上加读/写锁。锁文件不写入数据、不按文件创建注册表，锁由内核在进程退出时释放；极低概率摘要碰撞只增加串行等待。读共享锁，write/edit独占锁覆盖整个读取修改提交过程。符号链接解析到目标路径；任意bash、DAV、外部编辑器及运行中的浏览器不参与此协议。

write/edit先在目标同目录写临时文件、fsync，再原子替换；已有目标保留mode和UID/GID，新文件按umask创建。符号链接本身保留；硬链接的其他名字仍指向旧inode，已有打开句柄可继续读取旧版本。该接口不承诺保留扩展属性/ACL，不提供跨文件事务。正常失败或TERM取消清理自己的临时文件；VM断电/SIGKILL等异常终止可能留下 `.agent-space-write-*` 临时文件，但不会把未完成内容覆盖到目标文件。原子覆盖需要同时容纳旧、新两份内容。

## 5. Web 发布

`web_publish` 支持 publish/get/list/unpublish，一个空间内多项具名发布可以共存。每项有随机发布 ID、独立路径、目标、自动访问用户名和密码、单调时钟保活截止与撤销令牌。发布配置及凭据仅在当前运行 session 内存中，不持久恢复。file/directory 各持有自己的受管 static-http 实例；port 指向 Agent 已运行的应用。

```text
/u/{username}/pub/{name}/* → 当前发布/账号/认证 → 空间活动租约
    → guest 127.0.0.1:<port> → 流式 HTTP 响应
```

SessionManager 仍是唯一 VM 所有者。publish 在现有发布门闩中准备目标、复核账号并提交；失败只清理本次候选，不影响其他项。同名同目标延长保活，不替换入口或凭据；不同目标准备成功后原子替换，保留 URL、凭据、realm 及更晚的保活截止；切换撤销旧目标在途请求，失败保留旧项。helper 清理失败仍由空间持有句柄重试。取消、停止和删除一次性撤销全部发布和在途请求；单项 unpublish 只撤销该项。

默认保活 10 分钟、单次上限 1440 分钟。未到期发布同时阻止 session/idle 自动回收，候选收集与停止前复核都检查；多项取最晚截止。到期不删入口、不刷新 last_work；VM 在其他工作下仍运行时入口继续可用。正在传输的请求持有全响应生命周期活动租约，不长期占用 bash 队列或 Runtime 门闩。显式停止、禁用和删除不受保活阻拦。

发布与本次 VM 运行周期绑定：协议断开保留，暂停恢复保留，VM 停止/崩溃/重启或服务重启撤销，旧 URL 不启动 VM。单项关闭只清理自己的 helper，不删除用户文件、不停止用户应用、不关闭 DAV。源文件变更后读取新内容；目录边界由已打开根目录句柄约束。HTTP 认证、配置、返回及传输合同见 [接入参考](server-api.md#4-mcp-工具和-web-发布)，详细合同见 [Web 发布参考](web-publish.md)。

## 6. 删除、恢复与边界

删除适用于未创建、运行、暂停、停止、仅剩磁盘和无内存 Session 的空间，且不会为删除唤醒 VM。先原子记录 `deleting` 与精确资源清单，阻断新请求并取消在途工作，然后经 SDK 停止/销毁该 sandbox、根盘及登记的独立卷；确认全部不存在后才移除用户记录。部分失败保留 `deleting` 与错误，启动和运行期继续重试。未知 SDK 状态、权限或 I/O 错误不能当作 NotFound；未登记资源只报告，绝不依据用户名前缀猜测并删除。

备份必须同时包含 `state.json` 和 microsandbox 根盘/卷。恢复旧备份前须停止服务，并核对已撤销凭证及删除事实；只恢复磁盘不能让旧 Token 重新有效。宿主对 TLS、域名和外部网关负责。Hub 不可达不影响本服务独立启动；Core 用户状态改变也不会自动改变本地账号，若需要联动须由调用方完成跨服务操作与失败重试。

## 7. 配置与验证入口

Server 的状态、资源及管理员配置见[接入参考](server-api.md)，两种目标的完整模板、首次初始化和入口配置见[部署指南](deployment.md)。源码检查、回归矩阵和发布资格统一见[开发指南](development.md)；确切源码/镜像与现场范围见[验证记录](validation-records.md)。

## 受管进程

Server 在 MSB guest 中运行 helper，Standalone 在本容器内复用相同 helper、执行 owner 与身份保护，不另建任务监督器。这里维护 static-http、webdav 和 desktop 的共同分发、身份、启停及日志约定；“受管”由登记的实例句柄决定。

### 归属与所有者

“受管”由项目明确登记和持有实例句柄决定，不由进程名、运行用户或 Linux 父子关系决定。

| 对象 | 所有者与边界 |
|---|---|
| Host 容器中的 `agent-space-mcp`、可选 `sing-box` | 部署入口；沿用现有 Host 目录和启停方式 |
| msb、VMM、guest agent | microsandbox SDK/CLI；不重排其内部目录，不按名称清理 |
| Guest 中的 `static-http`、`webdav`、`desktop` | SessionManager 中对应空间的运行所有者 |
| Guest 中用户/Agent 启动的 Python、Node、开发服务器等 | 用户/Agent；关闭或升级 helper 不得停止这些进程 |

禁用空间、删除空间、回收整个 VM 会结束其中的用户进程；关闭一个 helper 只影响该实例。

### 目录与权限

```text
/opt/agent-space/
  bin/
    agent-space-static-http
    agent-space-webdav
    agent-space-desktop
  desktop/ # 与helper二进制匹配的内嵌Node源码
  etc/
    webdav.json
  README.md

/tmp/agent-space/<instance-id>/
  ...本次启动的身份、就绪信息、配置快照和有界日志
```

- `/opt/agent-space` 位于 guest 根盘，随根盘保留；内容由 Host 分发，可按当前配置重建。`bin` 放固定名称的二进制，`etc` 只放确有需要的持久配置投影，`README.md` 是 Agent 的稳定说明入口。
- `/tmp/agent-space` 使用当前已有的 `/tmp` tmpfs，不增加挂载。Server 示例上限为 512 MiB，Standalone 示例为 256 MiB；Host 在 VM 启动/接管后的准入准备中落实 tmpfs 容量，只扩容，不删除文件或自动缩小。每次启动生成新的实例 ID；运行记录不能当作跨 VM 重启有效的状态。
- 不引入 `releases`、`current` 链接、通用 manifest、独立持久日志目录或预留数据库目录。没有配置的服务不创建空配置文件。静态发布目标属于 session，不持久保存为下次自动发布的配置。
- `/workspace` 仍是用户文件卷，不存放 helper 二进制、配置或 PID。WebDAV 原子上传需要同卷暂存文件，保留路径及清理边界见 [WebDAV 参考](webdav.md#中断上传不得损坏旧文件)。

Server 中以上为 **guest 路径**；Standalone 复用同名容器路径，用户进程以 UID1001 执行，root 控制器持有私有运行记录。Host 镜像中的 helper 继续放在 `/usr/local/bin`，作为注入来源；Host 配置和 StateStore 沿用现有位置。路径同名不意味着 Host 与 guest 共享文件。

目录默认 `0755`、二进制 `0755`、说明 `0644`；配置、运行记录及内部日志仅授予启动器/helper 所需权限。当前 guest 可运行 root 命令，这些规则用于防误操作，不能构成对抗 guest root 的安全隔离。

桌面profile持久保存在 `/workspace/.agent-space/desktop/profile`；用户产物位于公开目录 `/workspace/agent-space/artifacts`。helper与worker仍在opt；任务JSON和日志仅在tmp实例目录，不作为跨重启任务恢复。Chromium、桌面应用和内部观察worker使用UID1001；用户脚本、截图保存和产物枚举复用空间文件身份，Server为VM内root，Standalone为UID1001。任务目录和结果校验使用同一身份，实例目录仅给遍历权限0711，身份/日志保持root权限。

### 配置与运行状态

Host 配置决定服务启用、外部地址、二进制来源和服务策略；StateStore 决定账号、凭证及资源归属。`etc/webdav.json` 只是非敏感配置投影，首次使用或配置变化时原子重建。Guest 手改文件不改变 Host 授权。

每次启动把本次所需配置固定下来，不在请求中反复读取可变配置文件。运行记录保存服务 ID、实例 ID、实际二进制摘要、PID/进程组、进程起始身份和监听端口；其中具体序列化形式由实现决定，不为每个字段建立一个文件。凭证、Authorization、完整上传内容不得进入 guest 配置或日志。

日志保留在实例目录并设置明确上限（每实例 1 MiB，超出轮转或截断）；Host 记录服务 ID、实例 ID、退出原因和必要的脱敏错误。故障保留的信息也必须有界；确认旧实例结束后清理其记录，不建立常驻日志数据库。

### 分发、升级与回滚

1. Host 验证来源二进制的内容摘要，复制到 guest `bin` 下同目录的临时文件；校验内容、设置权限后原子替换固定目标文件。不得原地覆盖正在执行的文件，也不靠时间戳或恒定 Cargo 版本判断内容一致。
2. 已运行实例继续使用原 inode，句柄记录它实际运行的内容身份。磁盘上的新文件不代表旧进程已经升级。
3. 下一次安全启动采用新文件。不得为了升级强杀正在传输的 DAV 请求；服务需要切换时先停止接受新请求，排空现有请求，再按实例身份停止旧进程。二进制、配置和说明必须与 Host 版本匹配。
4. 回滚由旧 Host 发布包重新注入匹配文件，不在 guest 内增加多版本选择器。状态和磁盘的回滚按部署文档恢复一致备份。

已有静态用户空间首次使用当前功能时按需注入 helper，不重建 VM 或用户卷。[旧静态迁移基准](operations.md#支持的静态发布基准)不包含 Web 发布和 DAV，因此不扫描或清理开发中间版本的旧路径。当前实例仍必须按 boot/start/inode 和标记检查归属，不能凭裸 PID 或端口终止进程。

### 生命周期合同

- SessionManager 仍是唯一运行所有者；服务 ID 为固定枚举，不接收用户提供的任意服务名、二进制或启动命令。每空间至多一个有效 DAV 实例和一个DesktopHub/desktop helper；每项 file/directory 发布持有独立静态服务实例，新增时沿用候选实例提交机制，不替换其他发布。
- 底层 engine 只提供安装、启动、检查、停止、guest TCP 等能力。实例句柄必须带服务类型和进程身份，不把 static helper 的路径写死在通用检查中。
- 启动成功必须通过实例身份和就绪检查；PID 文件存在或端口能连接都不够。Host 重连先核验原实例，再接管或停止后重启，不能直接再启动一份。运行时状态未知返回可重试错误，不据此删除或重建 VM。
- Host 重启后旧 Web 发布不恢复，其静态 helper 由接管流程清理。暂停或过渡状态的 VM 保留待清理意图，不为清理单独唤醒；首次恢复或重新连接后、接受新工作前完成清理。失败保留意图并拒绝本次准入，后续重试；成功后不重复清理新发布。
- 请求取消、启动失败、切换失败仍需保留可清理的句柄。停止时验证实例身份，再发 TERM、有界等待及必要的 KILL；失败保留句柄供重试，不清空后遗忘。
- 关闭静态发布不关闭 DAV；helper 重启不关闭用户应用。禁用/删除空间则撤销所有入口，并通过现有空间生命周期停止运行时。
- DAV 活跃请求必须占用活动租约；传输结束重新开始空闲计时。总 shutdown 必须有截止时间，不能被单个 helper 无限拖住。

回收 DAV 上传暂存时，先核对实例 boot marker，再预检所有目录项属于本项目普通请求文件，全部通过后才删除。归属错误、缺少 marker 且目录非空、未知条目均保留数据并报告失败；只允许删除没有 marker 的空启动残留目录。不能依赖目录枚举顺序先删文件后核验归属。

管理员资源摘要使用 `guest/workspace_stats.py`，由 Host 在已运行 VM 内以 `python3 -I -c` 一次性执行。它只核对 `/workspace` 工作卷并读取文件系统容量，不安装文件、不创建受管实例、不启动 DAV helper，不改变上述固定 helper 的归属。其截止时间由资源查询控制，不取得用户活动租约；不修改用户工作区内容。

桌面helper只管理自己的X11/LXQt/浏览器/worker子树，以PID+start身份保留后代并有界TERM/KILL。新helper内嵌五个JS文件，摘要同时覆盖worker，启动前原子安装；已执行任务不热换。普通bash的有界前台owner与固定文件操作脚本都是短命执行器，不是受管常驻服务。文件工具在 `/tmp/agent-space/file-operations.lock` 使用单个稳定的内核字节范围锁文件；不得在VM存活时删除该锁文件。write/edit的同目录 `.agent-space-write-*` 暂存随正常完成、失败或TERM退出清理。浏览器硬期限、控制权和300秒空闲详见[浏览器合同](browser-desktop.md)。

### 给 Agent 的说明

说明源 `agent-space/guest/README.md` 随发布维护，安装到 guest `/opt/agent-space/README.md`，说明：

- 这三个服务由 Host 管理；素材与产物放在 `/workspace`，用户自己启动的应用仍由用户负责。
- 保留目录、DAV 上传暂存目录的用途，不要修改或删除其中的项目文件。
- `web_publish` 只控制具名 Web 发布，不控制 DAV；不要手动启动第二份 helper、修改凭证或用 `pkill` 修复服务。
- 出错时提供可读的实例诊断，由 Host 的运行所有者恢复。

MCP `get_info` 的 instructions 只提供一句说明入口及最重要的边界，按需读取 README。不得覆盖用户的 `/workspace/AGENTS.md`，不得把长篇运维手册注入每次工具结果，也不得向 Agent 暴露管理凭证。
