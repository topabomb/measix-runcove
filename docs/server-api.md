# Server 管理与 MCP 接入参考

本页对应 Runcove Server 的管理与企业集成合同。Standalone 不提供这些管理 API，也不使用本页的多用户配置；共同工具行为及适配边界见[架构](architecture.md)。

本文对应当前 Agent Space 代码，面向管理员、MCP 客户端和 MEASIX Control Hub 接入方。状态所有权、VM 与 Web 发布生命周期见[架构文档](architecture.md)；镜像构建、数据卷、备份和验证见[Docker 部署](deployment.md)。

## 1. 初始化与地址

生产动态用户模式使用持久状态目录，并从环境变量读取独立管理员 Bearer：

```toml
[server]
listen = "0.0.0.0:8932"
public_base_url = ""

[state]
dir = "/var/lib/microsandbox/agent-space-state"

[admin]
token_env = "AGENT_SPACE_ADMIN_TOKEN"
```

`server.public_base_url` 默认留空，MCP/发布链接从实际入口生成；桌面使用相对路径。正式部署必须设置稳定HTTPS origin，并保留真实Host。DAV使用独立监听器（默认8933）、可选 `webdav.public_base_url` 和Docker映射需要的 `webdav.public_port`，主服务与DAV的路由不互通。空配置接受localhost及回环/私有/链路本地IP；正式域名需由对应公共基址明确声明。两个固定公共地址只决定规范链接和域名入口，不阻断本机/局域网IP访问，不信任任意转发头。不同发布仍共享主服务origin，仅发布可信内容。具体开发与正式配置见[Docker部署](deployment.md#2-配置与首次启动)。state.dir 须位于持久卷。正式启动前，在同一卷上显式初始化：

```bash
export AGENT_SPACE_ADMIN_TOKEN='replace-with-a-long-random-secret'
agent-space-mcp --config /etc/agent-space/config.toml init-state
agent-space-mcp --config /etc/agent-space/config.toml
```

`init-state` 只用于首次空状态目录；已有状态丢失时恢复备份，不创建空账号库。静态导入、动态 schema2 迁移与当前 schema3 升级是不同路径，按[运维手册](operations.md)核对源版本；当前升级保留原凭据和空间身份。

配置示例采用每 VM 3072 MiB 内存、`/tmp` 512 MiB tmpfs；临时文件按实际写入量消耗 VM 内存，完整停机后清空。内存配置只决定新 VM；已有 VM 需先正常停机、冷备完整数据卷，再通过官方 CLI `msb modify <sandbox> --memory 3G --max-memory 3G --next-start` 保存配额。先以相同参数加 `--dry-run` 核对计划，正常调用启动后生效；不直接编辑 SDK 数据库。

Host 在启动、重连或接管运行 VM 时，对配置中的 tmpfs 挂载进行一次有界检查，容量不足则在线扩容；保留文件、挂载身份及权限，不自动缩小容量。暂停/过渡状态不为检查单独唤醒，失败保留重试意图并拒绝本次准入。SDK 持久挂载配置不被修改，下次启动仍由 Host 落实容量。实际资源口径仍以注册 VM/工作卷及 guest 测量为准。

## 2. 用户路径和凭证

本地 `username` 唯一且不可改名，格式为 `^[a-z0-9][a-z0-9_.-]{0,63}$`，输入不自动规范化。一个账号对应一个持久空间 `agentSpaceId=spc_<uuid>`。

| 路径 | 访问方式 |
|---|---|
| `/u/{username}/mcp` | 用户 Token：`Authorization: Bearer <keyId.secret>` |
| `/u/{username}/pub/{name}/` | Basic 用户名 `webNN`、六位数字密码；或 `X-Publish-Key: <同一密码>` |
| `/admin/v1/users...`、`/admin/v1/status` | 管理员 Bearer；仅供可信后台或管理员使用 |
| `/healthz` | 服务级健康检查 |

MCP 无效或缺失 Token 返回 401；有效 Token 访问另一用户路径返回 403。每项 Web 发布以路径中的用户名和发布名选择资源，内部复核当前空间 ID，错误或失效发布返回 404；地址不含凭据。仅剥离匹配的发布前缀后转发；发布凭据不能授权管理 API、MCP 或 DAV。删除后同名重建会更换空间 ID 与磁盘；旧 Token、MCP session 和发布凭据均失效；重新发布同名可复用路径，但不复用旧认证身份。

## 3. 用户管理 API

所有管理请求携带 `Authorization: Bearer $AGENT_SPACE_ADMIN_TOKEN`。创建用户不会启动 VM：

```bash
curl -sS -X POST http://127.0.0.1:8932/admin/v1/users \
  -H "Authorization: Bearer $AGENT_SPACE_ADMIN_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"username":"alice"}'
```

201 响应返回 `username`、`agentSpaceId`、`mcpUrl`、固定的 `image`、派生 `browser` 和**仅此一次**的 `token`。创建可传image字符串或桌面预设对象，省略使用部署默认；已有空间不支持热换镜像。接口如下：

| 方法与路径 | 请求 | 结果 |
|---|---|---|
| `GET /admin/v1/status` | 无 | `ready`、`activeSpaces`；状态或后端不可用返回 503 |
| `GET /admin/v1/users` | 无 | `{"users":[...]}`；不含 Token |
| `GET /admin/v1/users/{username}` | 无 | 用户记录、运行状态、资源摘要、发布摘要与清理错误；不存在返回 404 |
| `PATCH /admin/v1/users/{username}` | `{"agentSpaceId":"spc_...","enabled":false|true}` | 禁用撤销 Token、关闭所有发布并停止 VM；恢复后仍须轮换 Token |
| `POST /admin/v1/users/{username}/token` | `{"agentSpaceId":"spc_..."}` | 返回新 `token`、`keyId`；旧 Token 立即失效 |
| `POST /admin/v1/users/{username}/dav-token` | `{"agentSpaceId":"spc_...","token":"可选的调用方凭证"}` | 首次签发或轮换 DAV key；省略 token 时随机生成，指定时设置；200 返回 `username`、`agentSpaceId`、`davUrl`、`keyId`、`token` |
| `DELETE /admin/v1/users/{username}/dav-token?agentSpaceId=spc_...` | 无 | 撤销 DAV key；已无 key 也返回 204，全局关闭时仍可撤销 |
| `DELETE /admin/v1/users/{username}?agentSpaceId=spc_...` | 无 | 提交删除意图并异步清理 VM、根盘和登记卷 |

列表和单用户 GET 含 `username`、`agentSpaceId`、`mcpUrl`、`status`、`hasCredential`、`hasDavCredential`、`stopPending`、`sandboxName`、`ownedVolumeNames`、`image`、`browser`；单用户 GET 另含 `workspaceStatus`，存在运行 session 时含 `publications`（具名发布的 `name,type` 列表），有清理失败时含 `cleanupError`。不回显用户、发布或管理员凭证。

DAV 状态/API、外部文件路由及活动租约使用同一空间权威；具体镜像与客户端实测范围见验证记录，未测 GUI 的兼容性不作承诺。全局 `[webdav].enabled` 默认开启；关闭时签发返回 409 `dav_disabled`，用户 GET 不返回 `davUrl`。开启配置须使用动态状态及独立监听器，GET 才提供 `davUrl`；公共基址留空时按实际入口及DAV外部端口生成。创建用户不产生 DAV key；MCP 和 DAV 各自轮换/撤销，互不代用。禁用/删除账号同时撤销两类 key；不存在返回 404，空间不匹配或账号不可签发返回 409。

- 创建同名用户返回 409。创建响应丢失时先 GET；若需要重新交付凭证，使用返回的空间 ID 调用轮换，旧 Token 随即失效。
- 所有单用户写操作核对 `agentSpaceId`；实例不符返回 409 `space_mismatch`，不能把旧请求作用于同名重建的新空间。
- 禁用停止完成返回 200；未完成返回 202 `{"status":"disabled","stop":"pending"}`。GET 的 `stopPending=true` 表示仍待停止，`cleanupError` 给出最近错误。停机未完成就恢复返回 409 `stop_pending`；服务会在启动及运行期重试停机。
- DELETE 的 202 仅代表删除意图已持久化；重复请求在清理中仍为 202。GET 返回 404 才表示资源均已清完，此后重复 DELETE 为 204。部分清理失败保留 `deleting` 与错误，服务继续重试。
- 管理凭证缺失或无效返回 401；一般错误体为 `{"error":"...","message":"..."}`。删除或取消在途命令不回滚命令已产生的文件副作用。


### 资源摘要

单用户 GET 返回独立的 `resources`，携带同一 `agentSpaceId`，沿用管理员 Bearer。列表 GET 和管理写操作的响应不包含该摘要，也不触发采集。首期只提供运行状态、配置额度、工作卷容量及有效时的客户机内存；没有 CPU 使用率、宿主驻留内存、镜像/根盘占用、历史曲线、批量查询、告警或计费。

以下为运行中的响应片段，所有容量及内存数值为**字节**，`observedAt` 为 **UTC Unix 毫秒**：

```json
{
  "resources": {
    "agentSpaceId": "spc_...",
    "runtime": {"status": "current", "value": "running", "observedAt": 1790697577596, "reason": null},
    "allocation": {
      "cpuCores": {"value": 1, "source": "sandbox", "error": null},
      "memoryLimitBytes": {"value": 268435456, "source": "sandbox", "error": null},
      "workspaceCapacityBytes": {"value": 268435456, "source": "volume", "error": null}
    },
    "memory": {"status": "current", "value": 17412096, "observedAt": 1790697577586, "reason": null},
    "disk": {
      "status": "current", "value": {"usedBytes": 8192, "availableBytes": 189628416},
      "observedAt": 1790697577766, "reason": null
    }
  }
}
```

- **运行状态**：`running / stopped / paused / transitioning / notfound / unknown`；`notfound` 表示账号存在但 VM 尚不存在，账号不存在仍为 HTTP 404。该状态与账号的 `active / disabled / deleting` 分开；`paused` 不表示宿主资源已释放。旧 `workspaceStatus` 与摘要使用同一次状态结果；查询失败时旧字段为 `unknown`。
- **配置额度**：CPU/内存读取 SDK 配置（优先 active config），工作卷容量读取 StateStore 精确登记卷的 metadata。仅资源尚未创建时采用当前创建默认值并返回 `source=default`。已有资源读取失败返回 `value=null, source=unknown, error=...`，不回退到默认额度。读取卷失败不抹掉 CPU/内存配置。
- **客户机内存**：`memory.value` 为配置内存减 MSB 明确提供的 `memory_available_bytes`。未提供可用内存或尚无样本时为暂无数据，关系非法时为采集错误，不把缺失值零当成测量，也不采用宿主 RSS。`observedAt` 是 **MSB 快照时间，不是客户机内存报告的更新时间**；返回前重新检查样本年龄，超过 5 秒的快照标为历史，不承诺精确瞬时值。CPU 百分比的有效性与实际窗口没有完整透传，本期不提供。
- **工作区磁盘**：只统计 guest `/workspace` 独立工作卷。先核对 SDK 挂载配置与登记卷，再用固定一次性 Python 命令检查 virtio-blk serial、挂载点和文件系统根，通过目录句柄调用 `fstatvfs`。`usedBytes=(f_blocks-f_bfree)*f_frsize`，`availableBytes=f_bavail*f_frsize`；与 DAV quota 口径一致，包含同卷上传暂存，不含根盘、镜像或宿主分配量。配置容量不保证等于已用加可用。统计时间是 Host 收到有效结果的时间。

| 观测 `status` | 含义 |
|---|---|
| `current` | 当前运行实例的有效观测；可能来自短时缓存 |
| `historical` | 保留旧值及原时间；`reason` 说明停止、陈旧、最新采集失败或运行变化 |
| `unavailable` | 暂无有效观测，例如未运行、未采样、采样关闭或采集繁忙；数值与观测时间为空 |
| `error` | 读取/校验/采集失败且没有可返回的旧值；数值与观测时间为空 |

常见 `reason` 包括 `stopped`、`paused`、`account_inactive`、`not_sampled`、`sampling_disabled`、`guest_memory_unavailable`、`stale_msb_snapshot`、`collection_busy`、`collection_timeout`、`workspace_stat_failed`、`runtime_changed`、`inspection_failed`。历史磁盘遇到采集失败仍保留原值和时间，通过原因与普通休眠历史区分；错误不会覆盖最近成功磁盘观测。采集后的状态复核失败也按读取错误处理，不能把未经复核的新值加入缓存，不能解释成空间已停止。数值零仅用于有效测量。

查询不创建 session、不启动或恢复 VM、不取得活动租约、不修改活动时间；不持有 SessionManager 运行锁，不阻碍空闲回收。停止/暂停/转换中或账号不可用时不进入 guest；运行中磁盘与内存独立采集，一项失败不影响另一项或用户详情 HTTP 200。采集后复核运行及空间身份，删除/同名重建的旧结果不能作为新空间返回；空间身份变化可返回 409，记录已删除则为 404。

首期固定 5 秒观测缓存、最多 4 个资源查询并发。同空间并发请求复用缓存；等待采集锁、单次元数据读取和各项采集均最多 3 秒，未命中查询依次执行前置元数据、并行采集、后置复核，不做后台轮询。超过并发上限立即返回繁忙状态。缓存以 `agentSpaceId` 隔离，运行身份变化使当前观测失效；每空间只保留一份最近成功磁盘观测。删除清除缓存，服务重启丢失缓存；停止空间无缓存时返回暂无数据。不会为了停机最终样本推迟停止，也不能把最近成功观测当成停机瞬间用量。

### DAV 凭证首次签发与轮换

上述 POST 同时处理首次签发和后续轮换。仅管理员 Bearer 可以调用；创建用户不会自动签发 DAV 凭证。主项目若提供用户自助入口，应在自己的后端核验账号/空间归属后代调，不向前端或 guest 暴露管理员凭证。

- **自动生成**：只传 `agentSpaceId`，服务端生成 32 个密码学随机字节的 64 位十六进制 token；每次调用都会轮换。响应丢失后不能查询原明文，可再次轮换。
- **调用方指定**：增加字符串 `token`，支持 32–256 个 ASCII 字符：字母、数字、`-._~+/`，以及仅位于末尾的 `=` 填充。建议用至少 32 个密码学随机字节编码生成；长度校验不证明随机性，不支持把它当成可记忆弱密码。不要复用账号登录密码、管理员秘密或 MCP token。
- **同值重试**：指定的值与当前有效 DAV token 完全相同时，返回同一 `keyId` 和 token，不写盘、不发送状态变更通知、不取消传输；自动生成 token 也可按原值重试。这只适用于它仍为当前值时，不是可跨多次轮换重放的幂等请求 ID。
- **换值或撤销**：更换为不同值时生成独立 `keyId`，旧凭证失效并取消旧请求；DELETE 合同不变。同一账号仍只有一个有效 DAV token。
- **无效输入**：空值、null、非字符串、长度/字符不合法及未知字段返回 400，不退回随机生成。非法 token 返回 `invalid_dav_token`，请求结构错误返回 `invalid_request`；均不改变旧凭证。
- **归属冲突**：指定值已是其他用户的有效 DAV token，或任何用户的当前 MCP token，返回 409 `conflict`；不泄露冲突账号或凭证。用户名和空间必须匹配，DAV 全局启用且用户可用；原有 401/404/409 规则不变。

Basic 的密码和 Bearer 的值都使用**完整 token 原文**。调用方不可从 `keyId` 拼接或推导自定义 token。明文仅在成功的签发/设置响应中返回，查询接口仍不返回 token 或摘要；同值重试的响应会回显调用方已提交的值。凭证只存于 Host 的权威状态，guest 配置不保存它。

## 4. MCP 工具和 Web 发布

自定义空间的五个工具为 bash、read、write、edit、web_publish。桌面预设空间另提供 browser_snapshot、browser_act、browser_read、screen_observe；`tools/list` 返回英文描述与完整参数合同，不依赖客户端保留instructions。镜像配置和浏览器参数见[浏览器合同](browser-desktop.md)。Web 发布的参数、路由、认证、生命周期和验收见 [Web 发布合同](web-publish.md)。

工具自身异常明确时直接说明原因和必要的修正方式；未知异常文本去除ANSI控制序列后限制为2048字符并保留头尾截断标记。命令输出、文件正文和已有产物沿用各自限额，不套用错误文本上限。read文本读取的offset/limit均为正整数，图片读取忽略行范围；目录与文件不存在分别报告，底层输出截断时明确提示缩小行范围；edit未匹配或多处匹配时引导使用当前精确文本或增加上下文。文件执行超时/取消保留清理确认信息，并提示重试前检查实际文件。

人工接管仅限制浏览器自动化，不阻断 bash/read/write/edit 或工作台文件上传。bash 仍按空间串行执行；文件工具可同时提交，每空间最多4项并行，超出排队，同一真实路径的写入/编辑互斥。文件工具总期限包含启动、排队与锁等待；未开始的超时请求不会迟到执行，已经提交后的失败不能一律视为没有副作用。基础工具共用原有VM生命周期与授权校验，具体文件一致性边界见[架构说明](architecture.md#基础工具并发)。

基础工具忽略未知字段，web_publish只解析所选action需要的字段；browser_snapshot、browser_read、browser_act及动作步骤拒绝未知字段，screen_observe忽略未知字段。已知但当前操作不使用的字段按[浏览器合同](browser-desktop.md#有序动作与任务控制)归一化。可选字段省略/null使用默认值，page_id/workdir/file/directory的空白占位字符串视为未提供；浏览器顶层可选view/op等选择项同样接受空白占位，并去掉两端空白。命令、文件正文、路径和替换内容原样保留。实际使用的字段仍校验类型、必需输入及目标冲突，不推测拼错的字段名，不把有效的wait_secs=0当成空值。web_publish采用平面Schema，仅接受keep_alive_secs，保活默认秒数由当前服务配置披露。Bash超时在入队前校验可表示的执行期限，超大值返回参数错误。

工具目录自身提供使用引导，不依赖 initialize.instructions。浏览器观察、动作、读取和截图使用单一四工具入口；公开参数说明给出对象、默认行为及常用下一步，任务和图像语义见[浏览器合同](browser-desktop.md)。

MCP 入口为 `/u/{username}/mcp`，使用 Streamable HTTP；不提供旧式 HTTP+SSE 双端点传输。Streamable HTTP 的响应可以是 JSON 或 SSE，客户端须接受两者。当前锁定 RMCP 1.7.0，服务端使用 SDK 默认协议版本 `2025-11-25` 和协商机制；请求 `2025-11-25`、`2025-06-18`、`2025-03-26` 时分别返回同一版本。

`bash` 的命令字符串使用必填参数 `cmd`；可选 `timeout_secs` 单位为秒，`workdir` 仅对本次调用生效。

客户端完成 `initialize` 后发送 `notifications/initialized`；后续 HTTP 请求携带初始化响应的 `Mcp-Session-Id` 和协商得到的 `MCP-Protocol-Version`，不能继续使用另一写死版本。初始化和目录发现不启动 VM。Host 重启结束已有协议会话，客户端须重新初始化；持久工作区和账号凭据不随协议会话结束而删除。

Web 发布的参数、响应、认证、配置和保活示例统一见 [Web 发布参考](web-publish.md#使用示例)。

## 5. MEASIX Control Hub 接入

以下是 Agent Space 对可信调用方的接入合同，不声明 Core、Relay 或 Android 当前版本的实现和部署状态；跨仓集成须按各自版本单独验证：

1. 可信后端配置 Agent Space 内部 API 地址和独立管理员 Bearer，只由后端调用 `/admin/v1/users...`。浏览器、Android 和 Runtime Relay 不持有管理员 Bearer。
2. 使用 Core 稳定且不复用的 `userId=usr_<uuid>` 原样作为 Agent Space 的 `username`。Core 可改名的用户名/显示名不能作空间键；本服务自行保存用户与空间关系，Core 不建第二张用户名映射表。
3. 显式开通时 POST 用户。201 返回的 `mcpUrl` 与一次性用户 Token 通过可信渠道交付直接 MCP 客户端。409 时 GET 核对已有记录，不自动轮换；创建响应丢失时按第 3 节恢复。
4. 需要联动禁用时先 GET 取得当前 `agentSpaceId`，再 PATCH `enabled:false`。200 表示停机完成；202 或 `stopPending=true` 时等待本服务重试。恢复须等 `stopPending=false`，PATCH `enabled:true` 后轮换并一次性交付新 Token。
5. 删除时先 GET 取得空间 ID，再 DELETE；202 后轮询 GET 至 404。旧 ID 对新建的同名记录返回 409，不能以新 ID 重试旧删除。跨服务调用失败的重试和最终状态由调用方负责，不能把它当成 Core 用户操作的同一数据库事务。

管理员可独立管理本服务，用户可直连 MCP。Core/Relay/Android 接入必须为每个用户传递其对应的 Agent Space 凭据并处理授权生命周期；管理员 Bearer 或企业共享上游 Token 不能代替用户 Token。接入方不得把 Agent Space 单服务验收当作整条链路已经通过验证。

## 6. 备份与验收边界

Server 备份同时覆盖账号状态与 MSB 根盘/工作卷；具体步骤见[运维手册](operations.md)。固定测试及现场范围见[验证记录](validation-records.md)，发布门槛见[开发指南](development.md)。本服务验收不能代替外部平台完整链路验收。

WebDAV 的客户端配置、能力和限制集中在 [WebDAV 参考](webdav.md#9-客户端接入与能力边界)。
