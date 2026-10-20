# Measix Runcove Web 发布参考

两种部署共用发布工具、认证和文件/端口代理。Server 使用 `/u/{username}/pub/{name}/`；Standalone 使用 `/pub/{name}/`，没有 VM 空闲回收，`keep_alive_secs` 不产生容器停机或链接 TTL，到期不撤销发布。独立配置不暴露 VM 保活策略；生命周期差异见[架构](architecture.md)。

本文描述当前工具、认证与生命周期合同。固定版本验收与部署身份见 [验证记录](validation-records.md)。

## 1. 工具、路由与身份

- 工具 web_publish，action 为 publish/get/list/unpublish；原生 JSON 参数，publish 提供 name 和 file/directory/port 三选一；get/unpublish只消费name，list不消费其他字段，无关参数忽略。可选项省略/null使用默认值，空白file/directory视为未选择；真实多目标冲突、必需输入缺失及实际使用字段类型错误仍报错。Schema保持平面结构，参数数量和名称不变。
- name 是空间内唯一的 1–64 位 ASCII 字母、数字、下划线或连字符，用于工具管理及公开路径；publicationId 是内部自动生成的实例 ID，用于 realm 和生命周期。
- 发布与主服务使用同一入口；server.public_base_url 默认留空，返回链接跟随当前MCP请求，显式配置时使用固定基址。路由为 /u/{username}/pub/{name}/...；不生成子域名、不要求通配 DNS/TLS。DAV使用独立监听器，不在发布入口提供DAV路由。
- 每项发布生成 Basic 用户名 web 加两位数字、六位数字密码；均保留前导零。同一空间的有效发布用户名不重复，跨空间可能重复，因此两位数字不是全局身份标识。同一空间最多同时分配 100 个发布身份；用尽时明确失败，取消发布后可复用。用户名与发布名定位发布项，内部复核当前空间身份，再校验该项凭据和账号状态。
- 每项发布使用独立 Basic realm（publicationId）。工具只返回带尾斜杠的根 URL；无尾斜杠的已存在发布先 308 到规范地址，再在规范地址 challenge，父路径不 challenge。
- 程序可用 X-Publish-Key 携带该发布的密码，并以 Authorization 携带应用凭据。浏览器 Basic 与应用自身 HTTP Authorization 可能冲突，不承诺透明叠加。网关凭据不转发到 guest；两种认证共用失败节流。webNN 配合六位数字密码的 Basic 格式保留给网关，即使用 X-Publish-Key 认证，也剥离浏览器可能夹带的其他发布 Basic。
- 同名同目标续期；同名不同目标先准备再原子替换，保留 URL、用户名、密码和 realm；失败保留原项。关闭后重建可复用 URL，但使用新 publicationId 和新凭据。
- publish/get 返回 name,type,url,username,password；list 返回相同数组但省略 username/password；unpublish 返回 name,status，重复关闭幂等。get/list 不唤醒 VM、不续期。

## 2. 生命周期

- 可选 `keep_alive_secs`，单位秒，默认和上限由当前配置换算后随工具目录披露；内置默认600秒，范围60..86400秒。省略或null使用配置默认值，非publish动作忽略。工具仅使用秒字段；服务配置仍保留分钟单位。
- 同目标 publish 将截止时间延到 max(原截止, 当前时间+请求时长)，不累计叠加；URL/密码不变。
- 存在未到期保活时，两条自动回收路径（session/idle）均不得停止 VM；多发布按最晚截止时间保护。截止判断使用单调时钟，不在工具结果中暴露内部截止时间。
- 到期只解除保活，不关闭发布、不重置最后活动时间。既有空闲回收与全传输活动租约继续生效；真实工作可以延长 VM 运行。
- 发布绑定当前 VM 运行周期。MCP/聊天断开不关闭；暂停恢复保留；VM 停止/崩溃/重启、禁用/删除和 Host 服务重启使发布失效；旧 URL 不冷启动 VM。
- unpublish 撤销该入口和在途传输，只清理本项受管 helper，不删除文件、不终止用户应用，不影响其他发布或 DAV。失败清理继续由原所有者持有句柄重试。

## 3. 目标映射与明确边界

- 文件：根 URL 只提供所选普通文件，实时重新打开，不暴露父目录；原文件变成目录、symlink 或特殊文件后返回 404。支持 MIME、下载文件名、GET/HEAD、单段 Range。
- 目录：发布前缀映射到所选目录，提供 index.html 或安全目录列表；拒绝穿越、symlink、特殊文件。helper 的目录补斜杠跳转必须保持在发布前缀内。
- 端口：只连接 guest HTTP 端口，不启动用户应用；剥离完整发布前缀后转发剩余路径和 query，并设置可信 X-Forwarded-Prefix。应用须配置外部 base path；不改写 HTML、JS、绝对资源 URL 或应用重定向，不为 /assets、/api 增加全局路由。
- Cookie 删除 Domain，并限制 Path 在发布前缀内以减少普通应用冲突；路径 Cookie、不同用户名和不同 realm 均不是浏览器安全隔离。禁止 guest 扩大 Service Worker scope 或发送全 origin 的 Clear-Site-Data。
- 同域不同路径仍为同源：已打开页面可能请求浏览器已认证的另一发布并读取内容，localStorage 等同源存储共享。只能承诺逐请求认证，不能承诺不可信网页之间、或其与同源 UI 之间的浏览器隔离。仅发布可信网页/应用；不要在同一 origin 放置依赖浏览器隔离的敏感 UI。外部跨 origin 请求仍检查 Origin/Fetch Metadata。
- 本机/局域网将 server.public_base_url 留空；正式部署明确设置客户端可达的稳定HTTPS origin，网关保留Host并转发到主服务监听器。无需单独发布域名配置。
- 请求/响应流式转发，不沿用 1 MiB 正文限制或 bash 总超时；建立 guest 连接有启动超时，上传、等待响应和下载使用 I/O 空闲超时。首期不承诺 WebSocket、应用守护或跨 Host 重启恢复。
- [web_publish] 配置保留 enabled（默认 true）、有限保活默认/上限、启动与 I/O 空闲超时；不再接受独立 public_base_url。

## 使用示例

```json
{"action":"publish","name":"report","file":"/workspace/report.pdf"}
{"action":"publish","name":"site","directory":"/workspace/site","keep_alive_secs":600}
{"action":"publish","name":"api","port":3000}
{"action":"get","name":"site"}
{"action":"list"}
{"action":"unpublish","name":"site"}
```

Server 的 publish/get 响应示例（Standalone 使用 `/pub/site/`）：

```json
{"name":"site","type":"directory","url":"https://space.example.com/u/alice/pub/site/","username":"web27","password":"038427"}
```

Server 配置如下；Standalone 省略 VM 保活的默认/上限字段：

```toml
[web_publish]
enabled = true
default_keep_alive_minutes = 10
max_keep_alive_minutes = 1440
startup_timeout_secs = 60
io_idle_timeout_secs = 120
```
