# WebDAV 参考

本文描述当前文件服务、认证、隔离与生命周期合同。目录遵循[受管服务规范](architecture.md#受管进程)，客户端配置和能力边界见[本页客户端接入](#9-客户端接入与能力边界)，固定版本与实际部署证据见[验证记录](validation-records.md)。

## 1. 职责与调用链

用于上传素材、下载产物和挂载浏览，并覆盖常用 WebDAV 客户端的网盘文件操作：目录浏览/新建、文件上传下载、覆盖、重命名、移动、复制、递归删除、元信息、Range 续传下载和锁。兼容目标是客户端连接本项目服务，不包括本项目挂载外部网盘；已有 rclone/FUSE 固定版本实测，不能据此宣称所有客户端兼容。双版本新验收按[开发与验收指南](development.md)执行，历史未测范围不自动豁免。DAV 与 MCP、用户进程访问同一 `/workspace` 卷，不复制工作区；Server 不从 Host 挂载正在运行的 guest 文件系统。不承诺 Office 协同编辑、跨 MCP/bash 的写锁、VM 离线时可读文件或永久在线服务。

```text
WebDAV 客户端
  → 独立文件服务域名 /u/{username}/dav/
  → Host DAV 认证、准入、活动租约
  → 现有 SessionManager 唤醒该空间、确保 webdav helper 就绪
  → 现有 guest TCP
  → agent-space-webdav → /workspace
```

实现由 Rust helper crate `agent-space/webdav`、一个 Host DAV 路由模块和现有所有者扩展组成。不引入另一套用户库、容器、端口池、任务队列、通用 supervisor 或后台同步服务。静态 Web 发布使用独立的只读 helper。

## 2. 配置、地址与浏览器边界

两种部署共用文件服务合同；本页涉及 StateStore、账号 API、VM 准入与 `/u/{username}/dav/` 的内容适用于 Server。Standalone 的入口为 `/dav/`，默认关闭，凭据来自独立 `RUNCOVE_DAV_TOKEN` 环境引用，无账号管理或 VM 生命周期；不接受 `public_port`，端口映射由编排负责。启用方式见[部署指南](deployment.md#独立单空间构建与启动)。

以下配置、URL 生成与管理流程适用于 Server：默认开启，只支持动态状态用户，不支持旧静态用户模式。

```toml
[webdav]
enabled = true
listen = "0.0.0.0:8933"
public_base_url = ""
# public_port = 18933 # Docker宿主DAV端口不同时显式设置
startup_timeout_secs = 60
io_idle_timeout_secs = 120
```

`public_base_url` 默认留空（也可省略），本机/局域网模式从实际请求入口生成地址，不固定网卡IP。启用DAV仍要求动态用户模式。`listen` 默认 `0.0.0.0:8933`，必须与主服务使用不同监听端口；`public_port` 缺省采用DAV监听端口，Docker映射到其他宿主端口时必须配置，例如18933。主服务返回DAV链接时沿用请求的主机和协议、替换为该DAV外部端口。

正式部署必须明确配置主服务和DAV各自稳定的HTTPS `public_base_url`，并使用不同主机或端口。公共基址只能是无凭据、路径、query和fragment的origin；DAV的HTTP固定地址仅允许localhost、回环和私有/链路本地IP。固定地址用于规范链接，并声明允许的域名入口；不禁止通过本机/局域网IP访问。空配置只接受localhost、回环、私有及链路本地IP入口，不接受任意域名或公网IP。
- 唯一外部根为 `/u/{username}/dav/`；用户不能选择其他用户、历史 space 或 guest 根目录。无尾斜杠的根在认证后内部归一化，不对有正文的方法做可能丢失正文的跳转。
- **实际路由由独立监听器隔离**：主服务监听器没有DAV路由；DAV监听器仅提供DAV和健康检查，不承载管理/MCP、桌面或Web发布。Docker必须分别映射两个容器端口，不能把18932/18933都映射到8932。正式网关按两个公共origin分别转发至对应监听器并保留Host；TLS外部协议由匹配的固定公共origin确定，不信任任意 `X-Forwarded-*`。仅生成两个不同URL不算隔离。
- Host语法、重复Host、绝对请求URI与Host不一致会被拒绝。浏览器Origin/Fetch Metadata按当前实际入口检查，Token、用户/空间、路径校验不变。DAV `Destination` 和 `If` 中的绝对资源URL须属于当前请求origin及当前用户DAV根；切换入口后不能继续携带旧入口的绝对目标地址。
- GET/HEAD 文件响应强制 `Content-Disposition: attachment`、`X-Content-Type-Options: nosniff`、`Content-Security-Policy: sandbox allow-downloads`；不提供 HTML 目录浏览页。不开放跨域 CORS，也不把 DAV 凭证存为浏览器 cookie。防止用户上传 HTML 后在文件服务 origin 执行脚本。
- 文件传输不使用命令执行总超时。文件请求流式传输，受空间已有磁盘容量、启动超时和 IO 空闲超时约束。IO 超时按实际请求/响应字节进展更新，不因总传输时间较长而终止；等待写入串行锁最多同一 `io_idle_timeout_secs`，超时返回 503 和 Retry-After；只有 PUT 放开正文大小，协议控制类 XML 正文仍限制为 1 MiB。

PUT 在正文读完前被拒绝时（例如 412 或磁盘写满的 507），helper 保持真实请求接收端，先返回原始错误，由现有连接所有者丢弃剩余上传。此阶段复用 `io_idle_timeout_secs` 作为固定总截止，后续字节不会延期；断连、取消、截止或服务关闭均结束读取并回收任务。它不缓存或重放上传，也不提交失败的临时文件。这样避免带未读数据关闭 TCP 时，代理偶尔把已有 507 误报为 502。

路径不是浏览器 origin 隔离边界，依据 [RFC 6454](https://www.rfc-editor.org/rfc/rfc6454.html)。Web 发布沿用主服务 origin，以 /u/{username}/pub/{name}/ 路由及独立 Basic 身份授权；合同见 [Web 发布参考](web-publish.md)。不使用发布子域名，不承诺发布之间的浏览器隔离。DAV 保留独立 origin。DAV/发布入口检查 Origin 和 Fetch Metadata，拒绝外部跨 origin 脚本请求，保留显式用户顶层导航；同源发布脚本可能读取浏览器已经认证的其他发布，真实浏览器验证应如实覆盖这一限制。

## 3. DAV 凭证与管理 API

沿用 StateStore 用户/空间绑定和 `davCredential: Option<Credential>`，不复用 MCP 凭证字段。管理 API 支持省略 token 自动生成，以及首次/后续指定 token；输入和重试合同统一见[接入参考](server-api.md#dav-凭证首次签发与轮换)。

自动生成和调用方指定的 DAV token 使用完全相同的存储/认证路径：`davCredential.secretDigest` 保存完整 token 的 SHA-256，`keyId` 由服务端独立生成，仅标识凭证实例和撤销范围，不能用来拼接 token。明文不落盘、不写日志、不下发 guest；此摘要方案面向高熵集成 token，不是用户弱密码存储方案。MCP 的凭证格式和接口保持独立。

| 管理接口（仍需 admin 认证） | 请求和结果 |
|---|---|
| `POST /admin/v1/users/{username}/dav-token` | JSON `{ "agentSpaceId": "当前空间ID", "token": "可选的调用方凭证" }`；首次签发或轮换，200 返回 `username`、`agentSpaceId`、`davUrl`、`keyId`、`token` |
| `DELETE /admin/v1/users/{username}/dav-token?agentSpaceId=...` | 撤销，匹配空间且已无凭证也返回 204 |
| 现有用户详情 | 增加 `hasDavCredential`；全局启用时提供 `davUrl`，不返回凭证摘要或明文 |

沿用现有管理错误风格：用户不存在 404，空间 ID 不匹配或用户状态不允许签发 409；全局关闭时签发 409，但允许撤销。创建用户默认只产生现有 MCP 凭证，DAV 需显式签发。

DAV 请求支持 Basic（username 为路径中的用户名，password 为完整 DAV token）和 Bearer（同一 DAV token）。两者都必须绑定当前启用用户、当前 space 和当前 DAV key；MCP/admin token 不能代用。缺少或无效凭证 401 并提供 Basic challenge，有效凭证访问别的用户 403；在认证失败时不启动 VM。

指定当前相同 token 是无写入/通知的重试，保持原 keyId 和在途请求。指定值已属于其他 DAV 用户或当前 MCP 凭证则返回 409。轮换为不同值、自动生成或撤销 DAV key 取消旧 key 的全部在途 DAV 请求，不影响 MCP 和预览；MCP key 轮换也不改变 DAV key。账号禁用/删除撤销两类凭证及所有入口。取消能终止传输，不能撤回已经完成的文件提交；连接中断后不得自动重放写操作。

### 已上线静态用户的导入

静态 `[users]` 导入、动态 schema2→schema3 和既有 schema3 Server 升级是三条不同路径。静态导入需停止服务、备份完整卷、`init-state` 后执行 `import-users --dry-run/--apply`，随后重新签发 MCP/DAV 凭据；已有动态状态升级则保留原凭据、空间身份和磁盘。当前 schema3 升级不改变状态版本，只转换配置。具体预检、冷备、升级及回滚见[运维手册](operations.md)。

## 4. Guest helper 的文件服务合同

协议实现固定为 `dav-server = "=0.11.0"`，锁入 Cargo.lock；当前使用 `agent-space/vendor/dav-server` 的可追踪本地补丁，修正属性写入、容量统计、普通文件覆盖顺序及读取错误和范围边界，补丁说明见该目录 PATCHES.md。复用它的 DAV 协议解析、响应与内存锁；实现项目自己的受限文件系统适配器及 PUT 提交包装层，不直接使用默认 LocalFs。非敏感 guest 配置仅包含配置 schema、固定 root `/workspace`、本用户 DAV prefix、锁期限和 I/O 空闲超时；从 `/opt/agent-space/etc/webdav.json` 生成实例快照。helper 绑定 `127.0.0.1:0`，通过受管实例就绪记录返回实际端口，不由用户指定。

| 能力 | 当前合同 |
|---|---|
| OPTIONS、PROPFIND | 正常 DAV 能力响应；PROPFIND 支持 Depth 0/1，infinity 返回 403 和 `propfind-finite-depth` 错误 |
| GET、HEAD | 流式、Range、正确长度和文件元信息；目录使用 PROPFIND |
| PUT | 完整文件写入；不支持部分 PUT/Content-Range（400），不支持 PATCH（405）；成功只在完整提交后返回 |
| MKCOL、DELETE、COPY、MOVE | 支持常规文件/目录操作、Overwrite 和条件头；递归操作中的部分失败保留 DAV 207 语义，不自动重试 |
| LOCK、UNLOCK | 同一 helper 内生效，刷新和解锁可在上传期间执行；请求锁期上限 1800 秒，实际期限在响应返回 |
| PROPPATCH | 支持 DAV:getlastmodified、微软 Win32LastModifiedTime（HTTP-date）及 ownCloud:lastmodified（Unix 秒）；先验证全部属性再修改，同批不支持项使其余项 424，不虚报成功；不保存任意 dead property |
| 修改时间与容量 | PUT 支持 X-OC-Mtime（Unix 秒），仅提交成功返回 accepted；COPY 保留源修改时间，MOVE 保留 inode 时间。PROPFIND 在根及子路径返回同一工作卷 quota-used-bytes/available-bytes，包含暂存占用，排除不可用保留块 |

Unix 秒时间取 0..253402300799，实际文件系统无法精确表示时失败，不静默接受截断后的时间。容量查询是当前工作卷的瞬时统计，不等于上传空间预留。

条件头中 `If-Match` 和 `If-Range` 使用强 ETag 比较；`If-None-Match` 使用弱比较，匹配时 GET/HEAD 返回 304，PUT 等修改方法返回 412，不能因客户端带 `W/` 而覆盖当前版本。畸形 ETag 不得导致请求任务 panic。

不宣称完整 RFC 4918 一致性。锁随 helper/VM 重启丢失，MCP/bash 不遵守 DAV 锁；这是文件交换模式的明确限制。客户端需能处理属性失败与锁丢失；GUI 实测范围见验证记录。

### 完整文件和范围读取

面向已经写入完成、长度不超过 **4 GiB（4,294,967,296 字节，含端点）** 的文件，使用既有 GET/HEAD 和 HTTP 单范围读取。长度、seek 偏移和范围计算使用 u64；有界块读取与流式代理不要求整文件入内存、预复制、完整哈希或媒体专用服务。更大文件没有新增拒绝限制，但不属于当前固定验收范围。

- HEAD 返回完整 Content-Length、ETag 和元信息，不读取文件内容；忽略 Range。
- 非空文件支持闭合、开放和后缀范围；超过文件末尾的终点和后缀长度裁剪到实际文件边界后计算计数，避免整数溢出。可满足范围返回 206 及准确 Content-Range/Content-Length；不可满足范围返回 416 和 `Content-Range: bytes */长度`。
- 畸形或无法表示的 Range 按当前实现忽略，返回普通 GET；数字只接受 ASCII 十进制数字。空文件忽略 Range，返回 200、Content-Length: 0，无 Content-Range，符合 [RFC 9110 的空表示处理规则](https://www.rfc-editor.org/rfc/rfc9110.html#section-14.2)。
- Range + 当前 If-Match 返回对应版本；旧 If-Match 返回 412，不自动提供新版本。If-Range 是另一种语义，不能代替客户端固定版本所需的 If-Match。
- 非空短读继续发送；尚有预期字节却读到 EOF，或后续范围 seek 失败，终止响应流并记录可定位错误。响应头发出后通过不完整响应/流错误表达失败，不补零、不重新发送状态行。首次 seek 失败按文件系统错误处理，不伪装成 416。
- Host 透传 Range、If-Match、206、ETag 和范围/长度头，传播 guest 读取错误。持续 I/O 进展不受固定总时长限制；背压、真正空闲、客户端断开、凭据撤销或空间停止按既有超时与取消机制结束，释放连接、文件及活动租约。

ETag 基于 inode、长度及纳秒时间信息，不是并发文件快照。已经进入缓冲区的真实字节可能在截短后继续到达；同长度原地改写或截短后再增长未必可检测，不承诺任意并发改写下稳定播放。Android 通过 Core 文件接口接入，负责响应校验、版本固定、重试/seek/缓冲与最终设备验收。暂停后可关闭旧请求并重新发起范围读取，无需无限保活连接。多范围保留现有实现，但不据单范围验收声称完成全部 multipart 兼容性认证。

### 目录隔离

helper 启动时打开 `/workspace` 根目录句柄；所有读取、枚举、创建、删除、rename/copy 使用相对此根的 fd 操作。拒绝所有路径组件中的 symlink、特殊文件、挂载越界、编码穿越及根目录 DELETE/MOVE。无尾斜杠及重复尾斜杠都识别为同一个根；根也不能作为 COPY/MOVE 的覆盖目标，必须在递归处理前拒绝。不得先 canonicalize 再用原始路径打开，以免检查与使用之间被替换。

目录 COPY/MOVE 不得覆盖源的祖先目录；MOVE 也不得移入自身后代。在任何目标删除前，按解码归一后的路径组件拒绝这些操作（403），保留源及目标内容。路径文本前缀相似不代表祖先关系，例如 `/tree` 与 `/tree2`；普通文件覆盖目录仍返回 409。

使用 Linux `openat2` 的 `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_XDEV`；对修改操作先安全打开父目录 fd，再做相对操作。实际 guest 内核须支持该调用；当前实现没有逐段 openat 或普通路径 API 降级，不支持时明确失败。参见 [openat2 手册](https://man7.org/linux/man-pages/man2/openat2.2.html)。

枚举隐藏 symlink 不等于禁止直达访问；文件系统适配器的 `open`、metadata、read_dir 及全部修改操作都必须执行同样的边界校验。原有用户 symlink 不删除，只拒绝 DAV 访问。

### 中断上传不得损坏旧文件

`dav-server` 当前 PUT 实现在接收前以 create/truncate 打开目标，而且 `flush()` 在 Content-Length 完整性检查之前执行。因此 **不能在适配器 flush/Drop 时直接提交**。依据该版本的 [handle_put.rs](https://raw.githubusercontent.com/messense/dav-server-rs/v0.11.0/src/handle_put.rs) 和 [文件系统接口](https://raw.githubusercontent.com/messense/dav-server-rs/v0.11.0/src/fs.rs)，当前提交顺序如下：

1. 每个修改请求携带请求级写入上下文。PUT 打开的是同卷 `/workspace/.agent-space/uploads/<instance-id>/` 下的唯一暂存文件，目标旧文件保持不变。临时目录需项目归属标记；已有同名非项目目录时拒绝启用并报告冲突，不接管或删除用户目录。替换上传需要旧文件和新暂存文件同时占用空间，不承诺原地覆盖的磁盘利用率。
2. RootFs 适配器写入/flush 只操作暂存文件；包装请求 body 记录完整 EOF、长度及取消状态。协议层仍执行锁与条件头检查，适配器提供真实目标的存在性及元数据语义。
3. 仅在正文完整、无取消且协议层得到 201/204 后，包装层 sync 暂存文件、复核目标条件，使用已验证的父目录 fd 原子 rename，再同步父目录。覆盖已有普通文件时保留其权限模式，新文件按明确的默认模式和 umask 创建，不递归修改用户文件权限。之后生成最终 ETag/Last-Modified 并返回成功。
4. 接收错误、容量不足、条件不满足或提交前取消，删除本请求暂存文件，旧文件不变。rename 后连接断开或目录 sync 失败属于“可能已提交”，不得回滚猜测或自动重放；记录可诊断结果。

旧 `/workspace/.agent-space-upload` 仅在归属标记匹配且新暂存目录不存在时原位迁入 `.agent-space/uploads`；无标记、归属不同或新位置已存在时不覆盖旧数据。现有受管实例先按原生命周期停止，清理器兼容两个位置的精确实例ID。旧位置不再作为DAV保留名称。

`/tmp` 是单独 tmpfs，不能用于保证大文件的同卷原子替换。只有根目录的精确子路径 `.agent-space/uploads` 是 DAV 内部暂存区：禁止直接访问或递归进入，枚举不返回它；递归 COPY 不带出暂存内容。MOVE 不能改名其父目录 `.agent-space`，避免移动活跃暂存位置；源或目标为该父目录（含编码及重复斜杠别名）时，在删除任何目标内容前返回403，保留源和目标。其他内部文件与目录按普通DAV权限操作，不隐藏或封锁整个 `.agent-space`。相似名称和普通子目录内的同名路径不受此规则影响。Core Files 对根 `.agent-space` 的隐藏属于 Core 入口策略，本服务不在基础工具或DAV上复制该策略。确认旧受管实例已结束后才回收其带归属标记的暂存文件，不能仅依据 PID 文件不存在判断。每个实例暂存目录持久保存 `boot-id`；整台 VM 重启后，仅在全局空间归属匹配、boot ID 与当前内核不同、条目都是本项目 UUID 普通文件时回收。相同 boot 或缺少启动周期证据的目录保留，由 typed instance 终止路径处理；不递归删除未知内容。

每 helper 使用按解码后的路径协调的修改预约，覆盖 PUT、MKCOL、DELETE、COPY、MOVE、PROPPATCH 和新 LOCK 的整个操作及惰性响应执行期。COPY/MOVE 一次预约源和目标；同路径及父子路径互斥，互不重叠路径并行。冲突请求按到达顺序等待，取消/超时/响应结束均释放预约；GET/PROPFIND 和锁刷新不被上传串行阻塞。此协调不约束用户进程。PUT 期间发现外部修改导致条件不满足返回 412，但不能承诺与任意 bash 修改形成事务。

原子替换覆盖完整文件 PUT 和普通文件 COPY：COPY 先在同卷暂存完成数据、保留源修改时间、复核源/目标，再 fsync/rename。普通文件 MOVE 直接同卷 rename 覆盖，不先删除旧目标；提交前失败时保留旧目标及 DAV 锁；rename 后 fsync 失败或响应断开属于可能已提交，不尝试猜测回滚或重放。文件覆盖目录返回 409，硬链接同 inode 的 COPY/MOVE 拒绝。目录覆盖及递归 COPY/DELETE 仍可能部分完成，不把它们描述为事务。保留正确条件头、错误及 207 结果并加入回归，见 [handle_copymove.rs](https://raw.githubusercontent.com/messense/dav-server-rs/v0.11.0/src/handle_copymove.rs)。

## 5. Session、准入与取消

现有 Session 持有固定 DAV 实例槽和启动 gate，复用空间 runtime、容量准入及生命周期，不另建 session map。入口为 `begin_dav_request`：

1. Host 校验用户、space、DAV key，在现有 admission 保护下取得 session，建立活动租约。
2. 按现有容量规则唤醒/恢复 VM；复用 runtime gate，但不经 `submit(None)` 排在用户长 bash 命令后。请求启动超时覆盖等待 gate、激活、安装和就绪检查，失败返回可重试错误。锁等待可在截止时间退出；已经取得 runtime 所有权的 VM/helper 启动由独立活动租约完成其有界操作，不能因 HTTP 超时或调用者消失直接丢弃启动 future。VM 激活完成时请求已过期，则保留 VM 句柄供重试，不再为该请求启动 helper。
3. 核验受管实例身份及配置；必要时安装、启动唯一 helper。连接 guest TCP 前再次核对账号、space 和 key，避免在唤醒期间被撤销。
4. 租约覆盖请求排队、上传、等待响应和响应正文，所有失败、取消和 body drop 路径都释放。内部工作活动计时扩展为 `last_work`，传输完成时重新计时；MCP ping 仍只算连接活动。

不得在整个文件传输期间持有 workspace runtime 锁。容量不足/暂时无法激活返回 503 和 `Retry-After: 1`；状态查询错误也返回可重试失败，不停止或重建空间。`web_publish unpublish` 不影响 DAV，Host 重启不恢复 Web 发布。

MSB 的 Linux 生命周期临时补丁负责旧 PID 运行身份判断及状态收敛，保留历史 PID；Agent Space 不直接写 MSB 数据库，补丁和上游跟踪见 [运行时维护](runtime.md#linux-运行身份)。已有 crashed/stopped VM 使用持久身份启动，paused VM 显式恢复；SDK 的并发启动结果只在权威状态确认 running/starting/paused/draining 时收敛，不能把 terminal 状态的 `SandboxStillRunning` 无限当成正在启动。异常退出后的显式工作盘锁可能比 runtime 锁稍晚释放；仅对明确的 incompatible-disk-mode 占用错误、且同一 sandbox 仍为终态时，在原激活预算内等待至多 5 秒再试，不解除锁或忽略真实共享冲突。启动失败通过不连接 guest 的元数据检查释放已确认 stopped/not-found 的容量；running、paused、transitioning 或未知结果继续持有名额。启动返回后、就绪检查前 VM 若已确认退出，同样立即释放名额并清除旧 guest 状态；身份未知仍保留容量。helper 启动失败保留实际运行的 VM 和待恢复实例记录；分发拷贝有 15 秒截止时间。

DAV 启动日志带 `username`、`agentSpaceId`、`sandboxName`、总耗时 `elapsed_ms` 和阶段耗时 `phase_elapsed_ms`。阶段区分 `wait_dav_lock`、`wait_runtime_lock`、`runtime_status`、`lookup_vm`、`vm_create`、`vm_start`、`vm_resume`、`wait_vm_transition`、`wait_disk_release`、`runtime_reconcile`、`guest_connect`、`prepare_guest`、`dav_helper_check/stop/start`；后续 TCP 建连或 HTTP 握手失败标记 `dav_guest_tcp`、`dav_guest_http_handshake`。`vm_start` 包含 SDK 自身的进程启动和首次 relay 就绪握手，`guest_connect` 指对已运行 VM 的连接。启动失败在进入 `runtime_reconcile` 前记录原始阶段，避免清理过程覆盖失败原因；重复报告不再覆盖首次失败记录。日志不包含 Token 或请求内容。

### 活跃工作区容量拒绝

仅当 Host 明确无法取得 `server.max_running_spaces` 限定的活跃工作区名额、且尚未向 guest 转发本次 DAV 请求时，响应为：

```http
HTTP/1.1 503 Service Unavailable
Retry-After: 1
Agent-Space-Error-Code: workspace_capacity_exhausted
```

`workspace_capacity_exhausted` 是稳定错误码，只表示活跃工作区名额已满。本次 DAV 操作尚未开始：请求方法、文件正文及修改操作均未转发到工作区。Core 可按 HTTP 503 与此响应头映射容量提示；释放名额后，客户端可重新请求。响应不包含其他用户、工作区身份或数量。已持有名额的活跃工作区仍可访问，暂停的驻留VM仍占名额。

其他503（启动失败/超时、连接故障、helper排空、状态不可读等）不带此错误码，继续沿用原有处理；不能从缺少此响应头推断操作未开始。该头由Host准入阶段生成，不接受guest响应提供同名保证，也不为已转发或提交结果不确定的操作添加它。

### 连接恢复及取消

VM 为 Running 不代表缓存 TCP AgentClient 可用；MCP exec 使用独立连接，成功也不能证明 DAV 连接健康。准入、helper 操作或 TCP 建连时确认底层 Agent 连接已关闭，当前请求返回可重试错误并标记 Session reconnect，下个请求由现有 VM owner 重新附着、重新核验受管 helper，兼容的实例继续接管。helper 健康检查、安装、替换和清理适用同一恢复规则；端口拒绝、业务错误和普通超时不替换缓存连接。

已发送响应头后正文断开，以传输错误结束，后续建连确认缓存关闭后进入上述恢复路径。HTTP 请求发送后不自动重试，包括 PUT/MOVE/DELETE；不确定提交结果由客户端核对。

TCP 写 EOF 使用 H2G BulkFinish 半关闭，继续接收响应；读 EOF 要经过 offset 校验。普通 terminal 后不再补发取消，读 EOF/terminal 后不再发消费 credit。取消中或已退休 correlation 的迟到 G2H finish 由固定 msb 运行时补丁处理，未知 correlation 和正常流校验保留。补丁范围及上游撤销步骤见 [运行时维护](runtime.md#relay)。

取消信号包含请求断开、DAV key 失效、账号/空间失效、Host shutdown。信号必须覆盖建连、上传、等待响应头和响应正文，并能主动关闭 guest TCP；不能只在客户端下一次读取响应时才发现撤销。Guest 观察连接关闭后取消未完成操作、清理暂存；已经提交的操作按上一节处理。启动中的请求被取消也必须留下可核验/清理的实例归属，不能产生遗失 helper。

停服先拒绝新请求并取消在途请求，再按实例身份执行有界清理；首期总预算 30 秒，部署停止宽限期至少 40 秒，不能按空间数量无限累加。`stop_all` 已统一使用此截止时间；清理失败保留明确错误，不把服务退出当作清理成功。Host 重连时先核验旧 helper，兼容则接管；不兼容则在安全排空/停止后重启，不并行启动两份。

## 6. Host 代理与 HTTP 语义

`agent-space-mcp/src/webdav.rs` 负责代理。DAV 使用独立的流式正文和超时策略，不使用 Web 发布的 epoch 身份。

外部 `/u/{username}/dav/` 前缀原样传给 helper，并作为其固定 DAV mount 前缀，响应 XML href 保持外部路径，避免字符串替换 XML。所有方法（包括 OPTIONS）先认证，首期认证成功后统一经 helper 处理。

- Destination 支持同 origin 的绝对 URL 或以 `/` 开始的绝对路径；拒绝其他 origin、其他用户、其他入口、userinfo、fragment、query、编码分隔符和越界路径。按同一套路径规则验证源与目标，避免重复解码。
- 解析 If 头中的 tagged resource URI，限定在同一 DAV 根；保留合法 ETag 和 opaque lock-token 条件，不能把锁 token URI 当作文件地址或粗暴删除 If 头。
- 保留 Depth、Overwrite、Timeout、Range、条件头、DAV 响应头和 XML 状态；禁止 207 被包装成管理 API JSON。只改写经过结构化验证、确实需要规范化的 URI。
- 移除外部 Authorization、Cookie、冒充内部身份的头、hop-by-hop 头和不可信转发头；Host 已完成认证，不把明文 DAV token 发到 guest。helper 仅绑定 guest loopback，不开放 Host 端口。
- 代理 body 流与连接任务必须共同持有取消信号和活动租约，响应发送结束后清理。网络错误不自动重试任何修改方法。

锁、条件头、递归失败遵循 [RFC 4918](https://www.rfc-editor.org/rfc/rfc4918.html) 中相应语义；本文明确收窄的属性、锁持久化和 Depth 能力保留兼容性限制，不因取消剩余客户端实测而扩大承诺。

## 7. 实现位置

`state.rs`、`workspace_service.rs` 和 `admin_api.rs` 负责凭证与管理合同；`session.rs` 和 engine 负责运行实例及活动租约；`agent-space-mcp/src/webdav.rs` 负责 Host 流式代理、认证、取消与 URI 限制。

文件布局保持直接：`webdav/src/main.rs` 负责进程入口和监听，`lib.rs` 负责协议限制与提交包装，`fs.rs` 负责受限文件系统和暂存，`mutations.rs` 负责路径修改预约；测试按隔离、上传和协议组织。受管进程身份和分发遵循统一规范，不另设 DAV 进程管理器。

固定版本验证结果集中在[验证记录](validation-records.md)，部署、静态账号导入和回滚集中在[部署文档](deployment.md)。

## 8. 验收矩阵与执行命令

| 层次 | 必测场景 |
|---|---|
| 状态/认证 | 静态导入、未知状态拒绝启动；MCP/DAV token 不互通；路径用户名错配；禁用/轮换发生在激活、上传和下载中；错误凭证不唤醒 VM |
| 文件语义 | 大于 1 MiB 的文件（至少 64 MiB）上传/下载 SHA-256 一致；慢传输超过旧执行超时仍成功；Content-Length 错误、chunked 中断、ENOSPC、断开保留旧文件；GET 只见 PUT 旧版或新版；Range、Unicode/空格路径、Overwrite、条件头、锁冲突/刷新、207 |
| 大文件读取 | 4 GiB 稀疏文件 HEAD、跨 2 GiB、尾字节/尾段、开放/后缀/极大终点、416、当前/旧 If-Match、空文件和畸形范围；确定性截短、短读与 seek 故障；实际对外代理链持续进展/空闲/80 次取消/撤销/禁用 |
| 隔离 | 所有动词的 symlink/编码穿越/替换竞态；Destination 与 If 跨用户/跨 origin；特殊文件和保留目录；上传 HTML 无脚本执行；经主服务 origin 访问 DAV、经 DAV origin 访问 Web 发布/MCP/admin 均被拒绝 |
| 生命周期 | 冷启动、容量不足、长 bash 并行、仅 DAV 活动超过 idle 阈值、传输结束后的空闲回收；断开/取消无租约及连接泄漏；VM/MCP 正常而 DAV 缓存关闭时恢复同一 VM/helper；helper 崩溃、Host 重连、未知状态保守失败、关闭 Web 发布保留 DAV 及用户应用 |
| 升级/交付 | 对已有工作区注入新目录，文件不变；当前 helper 身份核验和接管；两次重启无重复实例；停服有界；镜像内 helper 摘要与发布产物一致；备份恢复和凭证处理 |
| 客户端 | 协议回归及 rclone 上传/下载/实际 FUSE 挂载有固定版本证据；其他客户端按实际范围单独验证，不能声称所有客户端或通用挂载支持 |

故障注入和竞态必须保持为回归测试，不能仅用成功上传证明安全。配置、接口及命令变更须同步接入参考。

在 `agent-space` 下依次执行；源码测试不替代发布镜像验收：

```bash
export CARGO_BUILD_JOBS=2 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
cargo build -p agent-space-static-http -p agent-space-webdav --locked
cargo test --workspace --locked -- --test-threads=1
cargo clippy --workspace --all-targets --locked -- -D warnings
git diff --check
```

不并行运行完整构建或真 VM 套件。随后按部署文档执行 Docker、已有空间升级和真实客户端测试，记录镜像/提交/二进制摘要及结果。单元测试、mock、真 VM、Docker 和客户端验收分别记账，前一层通过不能替代后一层。

大文件对外 DAV 验证使用临时账号和真实 guest；先验候选镜像，更新后再验实际容器。`--full-read` 使用有界内存完整核对 4 GiB，其他范围测试只读关键偏移；`--stress` 增加 24 次并发提前拒绝的上传及 60 次上传/下载混合取消，检查缓存连接断开后的 DAV 恢复、MCP 可用、原文件和 helper/VM 身份保持及运行时合并错误为 0。连接探测恢复预算为 10 秒，已提交的失败上传不重放。验证脚本失败或清理失败必须退出非零并记录 `passed: false`，凭据不进入证据文件。

容量内大文件 PUT 另用 `docker/verify-dav-upload.py --image <image> --lan-host <Windows局域网IP> --evidence <report.json>`；`--container` 验实际部署。WSL 调用 Windows 原生 curl，以 stdin 传递随机测试凭据，连续 6 次上传 540,079,753 字节的非零内容，同时 3 路 PROPFIND。每次完整 GET 的 SHA-256、跨块和尾部 Range、目录全部 207，以及同一 VM/helper、MCP 与实际运行时摘要均需通过。候选使用隔离 512 MiB 内存、1 GiB 工作盘；既有容器只创建临时账号，原账号不写入，结束删除测试账号和本机载荷。


验证器同时核对镜像、缓存及真实 VM `/proc/*/exe` 的运行时摘要。`--identity-only` 只执行真实 VM 身份核验和临时资源清理，不代替 DAV 或故障验收。

```bash
python3 docker/test_verify_dav_large.py
python3 docker/verify-dav-large.py --image <候选镜像> --full-read --stress --evidence target/dav-review-verification/candidate.json
python3 docker/verify-dav-large.py --container agent-space-local --full-read --evidence target/dav-review-verification/deployed.json
```

## 9. 客户端接入与能力边界

Server 管理员先在动态配置中启用独立 `[webdav]` 监听器，再调用 DAV token 签发接口。Standalone 由部署配置启用并提供独立 DAV 凭据，使用固定 `/dav/` 地址；其 Basic 用户名固定为 `workspace`。Server 客户端填写响应中的完整 `davUrl`，用户名为本地 `username`，密码为完整 DAV token；不要填写 MCP token。支持私有局域网 IP 加端口的 HTTP 接入，外部网关可另行提供 HTTPS。该流程已通过本地 Host + 真实 VM 和候选 Docker 镜像的 rclone/FUSE 验证；不能代表全部 GUI 客户端通过验收。

rclone 使用 `webdav` 类型、`vendor = other`。运行 `rclone config`，按提示输入 URL、用户名和密码，以免把凭证放进 shell 历史。不要选择 Nextcloud 或其他厂商协议来请求本服务未实现的扩展；配置步骤见 [rclone WebDAV 文档](https://rclone.org/webdav/)。假设 remote 名为 `space`：

```bash
rclone lsf space: --recursive
rclone copy ./materials space:materials
rclone copy space:outputs ./outputs
rclone mount space: ./workspace-mount --vfs-cache-mode writes
```

Linux 挂载需要 FUSE；挂载写入先进入客户端 VFS 缓存，成功写回服务后才算远端保存，关闭挂载前须等写回结束。支持完整 PUT、Range 下载、目录/文件复制移动及删除；上传中断可重新上传，不能把下载续传等同于部分 PUT。服务不提供任意持久 dead properties 或跨重启 DAV 锁；支持 PUT X-OC-Mtime 和时间属性 PROPPATCH，具体能力仍取决于客户端是否使用这些扩展。挂载参数参见 [rclone mount 文档](https://rclone.org/commands/rclone_mount/)。

历史固定客户端实测：WSL Ubuntu、rclone v1.75.1，浏览/递归目录、中文/空格/#/% 文件名、空文件、64 MiB SHA-256 往返、复制/重命名/覆盖/删除，以及实际 FUSE 挂载随机读取、新建目录、写回、重命名和删除通过。同一流程也通过了 schema2 候选 Docker 镜像，身份和迁移证据见 [验证记录](validation-records.md)。这些记录不声明全部 Windows/GUI 客户端兼容；较新双目标文件/工作台验收按[验证记录](validation-records.md)各自范围识别。

可重复客户端验收工具为 `agent-space/docker/verify-webdav-clients.py`，需要 Linux rclone 和 fusermount3；它只创建并删除一个随机测试目录，凭证从环境变量读取。先在当前 shell 安全设置 `AGENT_SPACE_DAV_TOKEN`，再执行：

```bash
python3 agent-space/docker/verify-webdav-clients.py \
  --url https://files.example.com/u/alice/dav/ --username alice \
  --rclone /path/to/rclone --evidence /tmp/dav-client-evidence.json
```

报告同时要求文件操作、真实挂载及清理成功，不能用“仅列目录成功”代替客户端验收。


### 能力边界

当前实现面向工作区文件交换，可以承担通用 WebDAV 客户端的常规文件操作，但不提供网盘产品的全部能力。以下依据当前 `webdav/src/lib.rs` 的方法白名单、`RootFs` 适配器和已有回归；客户端兼容证据单独见上一节。

| 客户端操作 | 当前能力与限制 |
|---|---|
| 浏览、新建目录、读取元信息 | 支持；PROPFIND Depth 0/1，递归浏览须由客户端逐层进行；不支持 Depth infinity |
| 上传、下载、覆盖、中文及特殊文件名 | 支持完整文件；上传/覆盖有 64 MiB 往返证据，读取验证见下一行。64 MiB 不是最大文件限制，实际受磁盘可用量和网关限制约束；该客户端记录不代表多 GiB 上传或并发压力验收 |
| 下载续传、随机读取 | 支持 GET Range 和条件请求；已验证含恰好 4 GiB 文件的单范围读取。版本固定使用 If-Match，旧版本返回 412；客户端是否按此使用取决于实现 |
| 重命名、移动、复制、递归删除 | 支持；限同一用户 DAV 根内。普通文件 COPY 暂存后原子替换，MOVE 直接 rename，不先删除旧目标；目录递归仍可部分完成，非整批事务 |
| 挂载和客户端同步 | 实际 rclone/FUSE 读写通过。客户端可以逐层列举并比较 ETag/长度/时间；无服务端变更推送、REPORT/sync-token 增量同步接口 |
| 覆盖上传的数据保护 | 完整接收后原子替换，中断/容量不足保留旧文件；需要旧文件与暂存新文件同时占用空间；不约束任意 bash 并发修改 |
| 上传断点续传/分块合并 | 不支持部分 PUT、PATCH、可恢复上传会话或 Nextcloud 分块扩展。HTTP chunked 传输支持不等于断点续传；中断后重新完整上传 |
| 文件属性、时间戳 | 可读长度、ETag、Last-Modified；PUT X-OC-Mtime 保留上传时间；PROPPATCH 支持 DAV:getlastmodified、Win32LastModifiedTime 和 ownCloud:lastmodified；COPY 保留源时间，MOVE 保留 inode 时间。不保存任意自定义属性 |
| 文件锁及在线编辑 | 支持当前 helper 内 LOCK/UNLOCK/刷新，最长 1800 秒；helper/VM 重启丢失，MCP/bash 不受约束。不承诺 Office 协作或跨入口互斥 |
| 剩余空间显示 | PROPFIND 支持 DAV quota-used-bytes/available-bytes；根与子路径共享真实工作卷统计，包含暂存占用，排除文件系统保留块；不预留未来上传空间 |
| 并发写入 | 同路径/父子路径修改互斥，源和目标一次预约；无关路径并行。冲突等待有界且可取消；读取与锁刷新可并行，不能约束任意 Agent/bash 写入 |
| 网盘增值功能 | 无回收站、版本历史、分享链接、跨用户共享/细粒度 ACL、全文搜索、缩略图/转码、秒传、专有网盘 API |
| 文件类型与在线性 | 不提供 symlink/特殊文件/工作区外路径；访问会按需唤醒 VM，首次请求有启动等待，VM 无法激活时返回错误，不能离线读取 |

增量同步是独立的 [RFC 6578 扩展](https://www.rfc-editor.org/rfc/rfc6578.html)，Nextcloud 的可恢复分块上传也有[专门接口](https://docs.nextcloud.com/server/stable/developer_manual/client_apis/WebDAV/chunking.html)。支持基本 DAV 文件方法不代表实现这些扩展，也不代表 Nextcloud 等品牌客户端的专有登录和功能可直接使用。

适用范围是上传素材、下载产物、普通文件管理与挂载浏览。未执行的客户端场景不计为通过，协议能力不等于所有品牌客户端兼容。
