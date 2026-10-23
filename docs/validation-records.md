# 固定版本验证记录

本页保留源码检查、镜像资格和实际部署的证据索引，三者不互相替代。每份报告只适用于自己的输入和运行范围；本页不充当当前服务状态探针。接口与行为以对应参考文档为准，验证方法见[开发指南](development.md)。

公开报告可能是去除现场信息后的投影；源码和镜像摘要保留原值。标为原始输出的日志摘要不等于脱敏文件的当前摘要，私人部署快照不随仓库或离线包发布。

公开 Git 历史从整理后的首次提交开始，未保留此前的私有开发历史。下列旧报告及其证据中的 Git SHA 是当时输入的标识，不能在公开仓库中检出，也不能据此复现旧构建；源码摘要和镜像 ID 继续标识原始验证对象。首次提交包含之后的修复和文档整理，不能直接沿用旧镜像的资格结论；发布前仍须按当前源码重新构建并验证。

## MSB 0.7.9 升级交付（2026-10-23）

SDK、CLI、agent-client 统一锁定 0.7.9，两个 vendor 副本以官方 crate 为基础，仅保留配对的 Linux 生命周期临时补丁。上游已覆盖的 relay 回补、0.7.2 专用 CLI 修复工具及对应 Docker stage 已删除。SQLite 3.51.3 仍保留：上游内置 3.46.0 尚未修复独立的 WAL-reset 问题；来源、价值及逐项撤销条件统一见[运行时维护](runtime.md)。

最终镜像 `sha256:932d905bdc34f99160776980d7b9d3ca470c5903af26600649f33dcf86cc4e5a`，实际构建输入摘要 `38ddc5f7700bf5d2ae8b7e64e74e1f30d24222640151180d3b30cb06bccb9019`。镜像记录的 Git revision 是提交前基线 `8f9bd1aa291c7d32a23c729f0f984e5729d17726`，不能单独代表本次变更；正式构建固定提交归档，与前期调查使用的标签归档分别记录。升级演练基线重建了旧生产可写层中的实际公开程序与元数据，没有用旧基础镜像冒充当前生产程序。

同一候选通过完整 Server 生命周期（310 秒）、精确旧版迁移和独立卷完整冷备回滚、12 项 DAV/PID 复用与故障恢复、全部共享浏览器/图片/资料/文件/闲置套件、真实 VNC/键盘/剪贴板/观看控制/UI、4 GiB 完整读取及 Range、混合取消 60 次上传和 60 次下载、20 次真实写满，以及 10 轮双空间 SQLite/VM 重启。各资格资源清理成功；早期 DAV 演练泄漏的四个日志卷按测试身份核实后清理，检查器也已修正。构建内上游回归 260 项通过、8 项忽略；Rust workspace 330 项通过、0 项忽略，8 个 Python 套件、workspace Clippy、格式、Standalone 编译/Clippy/不链接 MSB 的依赖检查通过。忽略项不计通过；本次没有发布新 Standalone 镜像或离线包。

完整验收发现旧 keep-alive 参数、超时文本拼接 JSON、图片 files.bytes 字段、将动态耗时文本一并比较为进程身份等过时测试假设，均改为当前合同后重跑通过。HTTP 网络策略拒绝按 0.7.9 的结构化 403 验证，并用本机监听端口确认请求未到达主机。SIGKILL 故障夹具先观测内核进程确实退出，再验证首次 DAV 恢复；未声称覆盖信号送达至退出之间的竞态。独立子代理审查全部升级变更及后续修正，最终无未解决阻断项。

生产通过局域网更新：停止旧容器并关闭其自动重启，完整备份数据、日志、配置和代理配置，核对摘要并恢复至独立新卷，校验数据库后切换。旧容器和原卷保留用于完整回滚。更新前创建的临时空间在首次切换后以原 MCP 凭据读回工作区文件；随后现场 PROPFIND 检查器遗漏 Depth，断言失败并停止候选。修正为 Depth 1 并恢复服务后，仅轮换该临时账号凭据；公网 SSE 检查器另修正空数据帧及结束帧解析后，完成 MCP、DAV、双盘文件、桌面截图及公网 HTTPS 路径复验；再次重启后新临时凭据和双盘继续保留，实际 VM 可执行文件摘要与候选 runtime 匹配。旧 DAV 凭据跨升级的完整读取由隔离迁移资格证明，本次生产现场未完成该项原凭据读取，不将两者混同。临时账号及资源已清理，原账号状态不变，生产健康，相邻服务容器身份和镜像未变且最终处于运行状态。备份期间观察到相邻服务运行时长变化，本部署未操作它；未核实其重启原因，不声称全程无重启。现场地址、凭据、账号及资源标识不入公开证据。

[最终资格与审查](evidence/msb-079-20261023/qualification.json) · [生产更新与复验](evidence/msb-079-20261023/production.json)。

## MSB 0.7.9 升级调查（2026-10-23）

固定上游标签 `v0.7.9` / commit `ec5c6ecf0065bacaffc1b3752f3580ba20e1d179`，Runcove 基线 `8f9bd1aa291c7d32a23c729f0f984e5729d17726`。官方发布包、实际 VM `/proc/<pid>/exe` 和固件摘要匹配；在 WSL Linux x86_64 的隔离 MSB home 中验证，guest 内核为 6.12.113。源码、发布收录、测试名、日志摘要和边界见[验证证据](evidence/msb-079-20261023/verification.json)。

- 上游 relay 108 项、数据库/控制连接 29 项、执行会话 10 项、退出原因 1 项单元测试通过；另单独执行了默认忽略的真实 VM 闲置退出测试，1 项通过。其他忽略项不计通过。
- 官方 CLI 的 64 MiB 文件往返 SHA-256、并发执行、8 MiB 双向 exec、三次文件复制客户端取消后重连、detached job、暂停/恢复与停止/启动保盘通过。端口映射添加及移除经重启后验证 HTTP 可达性，映射变更不是无中断热更新。
- 终态 run 的历史 PID 替换为测试进程 PID 后，启动约 5.03 秒返回 `previous runtime pid … is still alive`，无关进程存活；恢复夹具后原盘仍可读。这是合成陈旧 PID 复现，不是真实内核 PID 复用，确认生命周期补丁仍有必要。
- 隔离 Runcove 副本只将三个 manifest 的 MSB 依赖统一为 0.7.9 并移除两个旧 MSB vendor 覆盖，应用 Rust 源码不改；解析新依赖后 `cargo check --workspace --locked` 通过。该调查阶段的正式仓库仍锁定 0.7.2，后续升级资格另记。

上述调查结果不等于 Runcove 新镜像发布资格。调查阶段未执行新版应用 DAV 4 GiB/Range、旧镜像升级、完整冷备回滚和桌面资格，也未部署。所有本次测试 sandbox 已清理；调查产物留在忽略的 `target/`。当前上游状态及逐项补丁撤销条件见[运行时维护](runtime.md)。

## 图片编码统一与精简披露（2026-10-21）

文件图片读取、浏览器标注快照及页面/桌面连续截图共用 `media.rs`：最长边2560、CatmullRom、WebP Q90、method=4，显式固定 alpha_quality=100、alpha_compression=1；小图不放大，缩放后的透明通道无损编码。保留原始PNG、解码及结果预算、控制权、任务缓存和取消归属。

工具说明去除缩放与编码参数；图片文本元数据只保留人类可读的size、format及必要的path/image_id/帧关联。宽高、原始尺寸、缩放、方向修正和首帧标记不再向Agent返回，坐标记录仍在内部维护。合并现有单元用例覆盖边界与alpha，删除一项重复的VM坐标用例；其余现场测试从实际图片字节读取尺寸，保持原图与坐标检查。

固定源码摘要 `a9e012896938b553478620def8fb23d5759263e19dcf6929ccaf7d4fbba6bef4`。快速验证通过图片单元5项、无默认特性MCP单元34项、MCP HTTP合同5项、helper单元9项，共53项；相关默认特性all-targets及无默认特性lib/bins Clippy、格式、Python/Node语法和diff检查通过。无默认特性Clippy首次误用all-targets，因既有MSB专属集成测试无法编译而失败；按文档范围重跑通过。生产二进制沿用缓存构建链导出，实际链接SQLite 3.51.3。

按用户明确要求直接更新现有生产MCP、desktop helper和release/runtime元数据，重启后安装程序、PID1及guest helper摘要一致。真实临时空间验证新读图、标注快照、长页底部坐标点击、透明/半透明像素、页面原图保存再读取的字节一致性、桌面连续截图及任务查询不重新编码。首次点击后截图返回STALE_OBSERVATION；另一次补验脚本对已经解析的Bash JSON重复解析，修正检查器后在同一部署程序上补验通过，没有再次部署。该次补验改用另一稳定页面，未定位首次STALE_OBSERVATION的原因；原始记录缺少前后状态差异。各次临时空间及上传暂存均已清理，原账号、配置、挂载、端口和相邻服务保持，服务健康。

同一临时读图样本的文本元数据由202降为70 UTF-8字节；实时九工具目录紧凑JSON由23335降为23083字节。这些是字节测量，不是模型token计数。首次执行时未提交或推送源码；后续复审和交付见下节。未新建资格容器、运行完整发布资格、重建发布镜像或离线包。生产为原容器可写层程序覆盖，按原镜像重建需重新应用或构建新镜像。

[快速检查与构建](evidence/runcove-image-policy-20261021/verification.json) · [生产更新与现场复验](evidence/runcove-image-policy-20261021/production.json)。

### 全部变更复审与定向生产更新

复审本轮全部待提交源码、测试、合同与证据。修正浏览器下载图片在files/downloads中仍暴露原始字节数、MIME和重复文件名的遗漏，补齐GIF产物的MIME登记；只处理受管图片列表，页面字段和下载文件内容保留。原图回归重新核对实际PNG与预览尺寸、认证下载哈希，避免移除公开尺寸后削弱校验。前次补验换页的边界已在原证据旁补充，未把它改写成首次异常已定位。

最终源码摘要 `c017de31745e27be8fac63aeb1f6994a5b9db2bbf33cdfc24384a34336912b1c`。同一最终输入通过Rust针对性53项、默认特性all-targets和无默认特性lib/bins Clippy、格式、Python/Node语法及diff检查；二进制实际链接SQLite 3.51.3。一次旧候选因审查期间源码变化而在替换前被拒绝；另一次远程编码命令超过Windows长度限制，改用经过SHA-256核验的上传脚本后安装成功。这些准备失败没有记作产品验证通过。

现有生产MCP与desktop helper定向更新，安装程序、PID1、guest helper及release/runtime摘要一致。一个新临时空间实际验证PNG/GIF读图及下载摘要、下载原始字节哈希、标注快照和万像素长页底部坐标点击、透明/半透明像素、同一长页的原始PNG保存与重新读取一致、桌面连续截图及任务取回不重新编码。首次点击后截图触发1次STALE_OBSERVATION，重新观察后同页截图通过；点击仅执行一次，重试只读观察。该保护逻辑未修改，前后状态差异未留存，根因仍未定位；此前本地Chromium的16次诊断截图未复现，不作为生产无缺陷证明。

临时空间及上传脚本/程序暂存均已清理，原账号、配置、挂载和端口保持，生产与相邻服务健康。准备期间观察到相邻服务启动时间变化，本轮所有写操作只针对Runcove。此次仍为容器可写层程序覆盖，未重建发布镜像、生成离线包或运行完整容器资格。

[复审与最终快速检查](evidence/runcove-image-policy-20261021/review.json) · [最终生产更新与现场复验](evidence/runcove-image-policy-20261021/review-production.json)。

## 浏览器最终 Git 审查与图片合同核对（2026-10-20）

审查本轮全部待提交源码、测试、资格脚本、文档及图片证据。修正升级/导入验证仍使用旧 action=wait、导入调用参数形状、DAV容量回归读取旧返回结构的问题；弹出页 URL 和截图 URL/标题截短时补回 truncated 标记；文档统一16动作、顶层op和精简快照语义，没有增加旧协议入口。

图片读取与截图均使用共享管线：仅当长边超过1980时进行一次等比例缩小，小图不放大；所有图片仍编码为 WebP Q90（method=6），保留透明度。因此1024×1536 PNG不需要缩小，但不能原样透传PNG；原文件和截图原始PNG下载保持原字节。旧生产读图PNG及截图JPEG不代表本轮候选已部署。

最终源码摘要 `a23c9bf33af9305b7696a2c3cc0bacaa447bd438ad90d09c528b29fbb8496310`。本次针对性验证：helper9项、MCP HTTP10项、协议8项通过；真实桌面Node35项通过、1项DPR2专属场景跳过；Clippy workspace/all-targets、格式、验证器调用回归通过。生产二进制以锁定Docker构建链构建，SQLite实际链接3.51.3，在Server基础镜像及Workspace中核对依赖。下方330项Rust和两入口完整共享回归属于其各自冻结输入，不改写为此次最终修补后的整轮资格。生产已按下段定向更新并验收。

提交 `0e613f8` 后按明确授权快速更新原生产容器，同时替换MCP与desktop helper并同步release/runtime元数据，未备份、未迁移、未快照，也未重建基础镜像。安装程序、运行PID1、helper及元数据摘要一致，原容器/镜像/配置/挂载/端口保持。更新前创建的临时空间在更新后继续使用原凭据和agentSpaceId，工作文件与浏览器页面恢复，helper及内嵌worker已升级；真实ref点击、iframe填写和读取通过。固定秒级等待及重复查询计时稳定；32秒间隔截图通过默认30秒任务等待完成，实际该次等待24.48秒。

同一张临时测试PNG（1024×1536，非用户引用的肖像文件）更新前read返回PNG，Base64为3186124字节；更新后返回WebP，为1573152字节（减少约50.6%），尺寸保持1024×1536。生产页面与桌面截图通过，保存原始PNG后read所得WebP与该次页面截图一致，长标题截断标记通过。临时空间及上传暂存已清理，原账号记录未变，生产和相邻服务健康。此为容器可写层覆盖，重建容器需重新应用或构建新镜像。[实际生产验收](evidence/runcove-browser-final-review-20261020/production.json)。

[审查与验证](evidence/runcove-browser-final-review-20261020/verification.json) · [生产二进制及运行库核对](evidence/runcove-browser-final-review-20261020/build-verification.json)。未重建最终镜像或离线包，未重新执行完整发布资格。

### 更新后生产调用复核（2026-10-20）

只读关联同一业务任务、已有空间、工具结果附件与服务完成日志，排除资格测试调用；解码图片头核对实际格式和尺寸。切换点为18:38:48（Africa/Nairobi，UTC+3）。此前肖像参考图read返回PNG 1024×1536，编码2092897字节、Base64 2790532字节；桌面screen_observe为JPEG 1440×900，编码173522字节、Base64 231364字节；页面截图为JPEG 1440×781，编码89534字节、Base64 119380字节。三次均发生在切换前。受查业务任务在切换后没有该肖像read或screen_observe，不能据此宣称该图的新WebP体积已实测，也不能套用上方临时测试PNG的压缩比例。

已有空间在切换后启动新helper，成功执行wait.secs=5，返回新版计时字段及duration_secs=5.1。切换后15次业务浏览器响应中，11次完成成功、1次初始运行中、3次错误；两次仍传已移除的action/ms参数，一次查询报告原点击任务的STALE_REF（completed=0、side_effects=not_started）。调用方改用新参数、重新快照取得ref后成功。旧参数来自目录缓存还是对话历史尚未确定；建议协议更新后重新发现工具定义，页面恢复后重新获取ref，不增加旧协议兼容入口。

[脱敏调用复核](evidence/runcove-browser-final-review-20261020/production-call-audit.json)保留输入版本、图片字节统计、恢复证据与检查边界；原始业务消息和标识仅保存在本机运维目录。本轮未修改服务实现、未触发新的业务调用，也未重新执行发布资格。

## 浏览器工具统一接口与完整验证（2026-10-20）

本轮完成四个浏览器工具的最终接口与 Agent 披露调整，同时完成下方五个基础工具首次快改的后续验证。browser_act 说明连续多步执行和通用固定等待，顶层统一 op=execute/wait/cancel；动作由22种合并为16种，旧动作名和旧顶层 action 入口已删除。click/drag 统一 ref 或图片坐标；press/scroll 使用 page/desktop 目标；navigate 使用 open/back/forward/reload。已知但无关的步骤字段先归一化再验证，真实意图冲突仍报错。

固定等待采用0..60整数秒，条件等待默认60秒。纯延时使用原任务、控制权、取消及期限，不访问页面，也不唤醒休眠桌面；两入口均实际验证成功。页面截图明确为当前顶层文档完整布局，独立滚动区域和 iframe 仅显示当前区域，不扩展懒加载或虚拟列表。清除 schema 自动标题、数值实现格式、内部 ref 格式及无意义 null 默认；四工具实际 MCP 目录由21427字符缩为18681字符（约12.8%，字符统计，不冒充模型 token 数）。普通结果不附页面列表，保留必要计时、失败原因和产物；URL/标题限长并标记截断，下载文件内容不截断。

write 在 MCP 与执行入口拒绝空 content；图片读取忽略文本行范围；发布工具使用 keep_alive_secs，旧分钟解释路径已移除；bash 返回开始时间与秒数。文件回归另修复目录读取在 fdopen 前校验同一 fd 类型，恢复 NOT_A_FILE 合同。

验证：Rust 全工作区330项通过；Standalone 无默认特性库34项通过，最终 MCP 集成10项通过；Clippy 工作区 all-targets 与 Standalone 无默认特性 lib/bins 通过；Node 浏览器/worker35项通过，普通运行有1项DPR2场景不适用跳过，另以DPR2专项2项通过；Python22项通过。所有发布二进制构建完成，动态依赖可解析。Server 共享回归48项通过；Standalone47项通过、1项Server升级专属场景跳过，另有独立入口、权限、控制进程异常和重启检查通过。两入口均覆盖真实60秒等待、真实300秒资料恢复、冻结浏览器清理、被动VNC休眠、1GiB文件流，以及页面上限4→1重启后的已有页保留、拒绝新增、复用与关闭。各入口整轮单次通过、资源清理确认；实际披露的四工具目录完全一致，共享helper摘要一致。

冻结源码摘要：`6f4beb28654039b4cfb17a2ac7e7b21dc1a2ecba20335ecd031607856d3487e7`。真实回归使用独立基础镜像及冻结开发资产；Standalone 实际使用 --no-default-features 构建输入。此结果是当前源码与二进制的回归证据，不等于最终发布镜像资格，也不等于外部 Google Flow 登录、风控或付费生成成功；未部署生产。

[完整验证](evidence/runcove-browser-disclosure-20261020/verification.json) · [最终四工具定义](evidence/runcove-browser-disclosure-20261020/browser-tools.json) · [审查明细与边界](evidence/runcove-browser-disclosure-20261020/review.md) · [源码与发布二进制摘要](evidence/runcove-browser-disclosure-20261020/source-and-binaries.json)。各入口原始通过报告与日志在同目录 server/、standalone/ 中；该目录不包含凭据、用户数据库或浏览器个人资料。

## 五个基础工具披露与容错调整（2026-10-20，首次快改、当时未验证）

此为首次快改时的历史状态；最终调整、兼容路径删除和后续验证已由上方完整回归记录覆盖。

按用户要求快速修改 bash/read/write/edit/web_publish：精简说明和类型元数据，在目录中披露配置的实际超时和保活默认值；图片读取忽略文本分页参数，multimodal 的 null 按 false 处理；发布使用 keep_alive_secs，当时保留旧分钟调用兼容（后续已移除）；bash 附秒级开始时间及耗时。write 在 MCP 及文件执行入口均拒绝空 content，保留非空文本的原始空白；edit 的空替换仍表示删除。四个浏览器工具的描述和参数定义未改动。

本次遵照明确指令，没有编译、运行测试或执行验证。上述改动不继承下节固定输入的资格结论。

## 浏览器与图片联合复审、秒级等待和精简反馈（2026-10-20，本地）

复审合并后的浏览器和图片改动，修正长页缩放取整后的纵向点击偏移、观察子进程数字退出码泄露，以及任务状态读取短暂失败就取消原动作的问题。状态读取失败只继续查询同一任务，仍受原期限和取消约束，不重放动作。另修复降低页面上限的测试夹具混用崩溃资料、文件校验依赖桌面及旧协议字段断言。详见[复审记录](evidence/runcove-browser-combined-20261020/review.json)。

面向 Agent 的可配置时间改为整数秒：固定等待0..60秒，条件等待默认最多60秒；元素操作默认5秒、下载默认30秒、连续截图间隔默认1秒。整批任务仍默认120秒，最多600秒；首次调用8秒未完成返回任务ID，后续查询不会续期或重放。默认快照和动作反馈不重复标签页清单，显式 `browser_snapshot(view="tabs")` 查询；常规成功结果省略健康检查、内部阶段和原始日志，保留真实字段值、截断、失败位置、控制权与必要副作用。已接纳任务有 duration_secs；仅动作、连续截图和运行中/异常任务附 started_at，普通成功观察/读取/单图省略，查询沿用原任务计时。现行合同见[浏览器工具](browser-desktop.md#浏览器与截图工具)。

最终产品代码对应源码输入 `5a201196b227f00b7b4d0ed5305ab375037a71c66b03d33b2071d34058b64689`；收尾仅把工作台测试中的两个 wait.ms 调用改成 wait.secs，输入摘要为 `655fa29ea8493690085bf78029641a030525e597dd73176fdbb3601202005022`，产品源码和二进制未变，逆向替换后的摘要核对一致。完整 Rust workspace 329项通过；最后计时可见性和任务监督调整后，受影响模块77项、两种feature的Clippy及入口构建通过。真实Chromium组件和故障回归32项、专用DPR=2两项通过；常规套件中1项DPR=2用例不适用而跳过。Python常规40项通过、5项环境专属跳过，另在root容器中9项通过覆盖这些分支；UI构建和release程序构建、动态依赖解析通过。详见[验证摘要](evidence/runcove-browser-combined-20261020/verification.json)。

Server共享功能47项覆盖通过（七组套件42项，加独立工作台补验5项）；Standalone完整共享功能46项通过、1项Server旧身份升级专属用例跳过。两目标均经历真实300秒的多资料恢复、冻结浏览器回收恢复和被动VNC休眠，验证原页面、所选资料和localStorage；1 GiB文件流、真实键盘/剪贴板、控制权、代理边界与重启降额4→1均通过。降低上限后四个旧页全部保留，新增拒绝，原页可导航和关闭。Standalone另通过执行身份、控制进程丢失、SSE关闭及重启持久化检查。两目标工具目录与共享helper摘要一致，所有验收容器和卷清理通过。

实际固定60秒等待在Server/Standalone约61.1/60.9秒，默认条件等待超时约61.4/60.9秒，均含调用和处理开销。真实MCP断言覆盖普通成功观察不返回开始时间、动作和连续截图返回开始时间、重复查询终态耗时不增长，以及快照不夹带页面清单。目录紧凑JSON为21427 UTF-8字节，不是模型token计数。当前页、显式页列表及查询样例见[实际返回](evidence/runcove-browser-combined-20261020/compact-feedback.json)。

中途失败均保留原始结论：配置档测试仍等待已省略的正常health.page、工作台仍携带旧ms参数，分别修正为公开page_id就绪判断和秒级调用；不恢复冗余字段，不放宽恢复/权限断言。此前VNC唤醒取消的历史原因未能确定；本轮修正已证实的“单次状态读取失败就取消任务”逻辑，并在两个入口真实暂停helper五秒，确认动作完成且点击仅一次。Server失败报告仍为passed=false，补验独立记录；不把它改写成单次完整通过。

本轮使用独立容器中的当前开发资产。Server 由七组共享套件及另一次工作台/重启补验组成，产品源码和二进制一致；源码摘要的唯一变化是工作台测试的两个等待参数，补验报告 `shared_full=false`。Standalone 独立运行完整共享套件。所有开发资产报告均为 `full=false`，不等同于最终发布镜像资格。没有更新生产、提交或推送代码。

## 浏览器参数容错、等待语义与文本精简（2026-10-20，本地）

按生产失败类型补齐参数归一化：execute 忽略 wait_secs，wait/cancel 忽略执行上下文与空 actions，query/extract 忽略 max_chars；保留新动作与任务控制等真实意图冲突。query 支持 ref 子树；条件等待覆盖可见文本与可访问名称；精简常规元数据、浮层重复内容和重复 stderr，保留控制权、失败现场、完成步数与副作用。字段名和四工具入口保持一致，具体规则见[浏览器合同](browser-desktop.md#有序动作与任务控制)。

两种 feature 各通过 MCP 单元31项、HTTP合同5项；workspace/all-targets Clippy（-D warnings）及两入口构建通过。真实 Chromium worker 25项通过，1项专用 DPR=2 用例本轮跳过；完整 worker 运行使用 HEADED=1、Xvfb/openbox 和512 MiB共享内存，初次64 MiB容器在大页面截图时崩溃不计入通过结果。独立 Standalone 与 Server真实MSB VM 各通过工具目录校验和结构化浏览器10项，包含新增容错/名称等待及并行图片改动的整页底部、缩放坐标回归；控制器、工作区及协议检查按各自报告列出。两目标均使用 helper `43c58f9e6454d8259bdcdb934016e934cb7431b9ca0c13103d36a26ef6b0f019`，隔离容器和数据卷已清理。

固定样例的 query、填写后反馈、菜单快照文本 JSON 分别从402/520/1329字节降到216/297/892字节（减少46.3%/42.9%/32.9%）；四工具目录紧凑JSON从21972降到21151字节（减少3.7%）。这是UTF-8字节测量，不是模型token计数或全站平均。源输入、二进制摘要、最终目录、专项运行器及测量样例见[验证摘要](evidence/runcove-browser-tolerance-20261020/verification.json)。

本轮是本地开发资产的工具合同专项，不是完整发布镜像资格；未重跑300秒资料恢复、Page上限重启恢复或完整UI/文件压力。截至该次专项，下节图片任务记录的重启恢复失败尚未解决；本轮不以专项通过覆盖它。后续排查与完整功能覆盖见[后续联合复审](evidence/runcove-browser-combined-20261020/verification.json)。未更新生产，也不通过工具完成状态判断Flow等网站的业务提交成功。

## 图片统一编码与原始 PNG（2026-10-20，本地）

截图先取得原始 PNG；快照、页面/桌面截图与文件图片读取统一走 `media.rs`，一次 Lanczos3 等比例缩小到长边≤1980，返回 WebP Q90（method=6）。保存原图保留 PNG；任务查询不二次编码；连续截图按实际累计字节计算预算。补齐公开帧的 scale，并修复取消时已保存原图与返回帧数不一致的边界。现行规则见[截图合同](browser-desktop.md#截图与连续观察)。

固定验证输入 SHA-256 为 `bdbc18573c5dfdc841b8ba88b5a32c1209f2e8be02178d2aee17a1e4480379c6`。图片单元5项、helper9项、MCP单元与合同37项、Chromium常规22项和原生DPR=2两项通过；最终独立Standalone容器中的图片、文件和坐标专项12项通过，Clippy、格式和Python语法检查通过。中文密集页面原始PNG为2200×1450、463547字节，返回WebP为1980×1305、205990字节，已目视检查。见[示例图](evidence/runcove-image-webp-20261020/chinese-text.webp)与[验证摘要](evidence/runcove-image-webp-20261020/verification.json)。

附带的降低Page上限后重启恢复检查失败：预期4页，实际1个新标签页；本轮未修改或调查该功能，因此不标记整套发布资格通过。该历史失败的后续排查与修复验收见[后续联合复审](evidence/runcove-browser-combined-20261020/verification.json)。测试容器与数据卷已清理；本轮未部署生产、未构建新发布镜像或进行新的Server VM资格验证。报告对应固定二进制；随后并行推进的浏览器合同改动需另行验证。

## 结构化浏览器四工具替换（2026-10-20）

旧工具集最后版本已独立提交并推送为 `bdee824e762e913032d540733bc8f4c1472d04f2`；本轮在 `codex/structured-browser-tools` 上，以 browser_snapshot、browser_act、browser_read、screen_observe 完整替换旧入口。观察、读取和动作使用短生命周期 worker，任务、控制权、Page 准入与生命周期继续由既有 Rust owner 负责。快照默认只返回语义，显式 image=true 附带 ref 标注整页图；页面截图默认完整已渲染文档，复用同一坐标映射。合同见[浏览器与云桌面](browser-desktop.md#浏览器与截图工具)。

完整功能验收固定输入 SHA-256 为 `0ce59840fc600d3c789929d308af0c6b0e57c30757f019c2028f85affd117845`，两种运行模式绑定同一新版 desktop helper，SHA-256 为 `25d2272575edf69fecc5eb957d147ee990de38b01deddff1ff0dabdb340c140c`。完整 Rust workspace 串行323项、最终helper8项、真实 Chromium 组件23项（含原生DPR=2）、两种构建方式的 Clippy 和无MSB Standalone入口构建通过；未改动输入的Python26项和UI类型/i18n/DOM/构建检查沿用本轮先前通过结果。程序、UI、测试输入和日志摘要见[验证摘要](evidence/runcove-structured-browser-20261020/verification.json)。

独立 Server、Standalone 均完成整套共享功能验收：Server40项通过；Standalone39项通过、1项Server旧UID1001资料重开专属测试跳过。覆盖同源/嵌套/跨域OOPIF、动态iframe、窗口及命名弹窗、JS菜单/tooltip/popover/模态框、shadow DOM、已打开的原生JS对话框、ref替换失效、字段真实值及Unicode边界、整页底部像素与坐标、整批校验、稳定失败状态、超时/取消/断连不重放、上传下载和1GiB文件流。两目标均通过实际VNC键盘/剪贴板/控制权/第二观看者/窄屏界面，以及分别经历真实300秒的多资料恢复、冻结浏览器回收恢复和被动VNC休眠；重启降低Page上限4→1后，原四页保留，新增拒绝，原页仍可导航与关闭。Standalone执行身份、秘密与文件权限、控制进程丢失及重启边界通过。两套工具目录一致，隔离容器和命名数据卷清理成功。

复验修正了弹窗压力夹具的一次手势批量调用；四页冷恢复实测首次附着约7.9秒，将单次附着上限5秒调为15秒，仍受原任务期限和取消控制，没有重试或常驻连接。旧冻结夹具误把 desktop=null/preparing=true 当作休眠，并过早解除SIGSTOP；现只接受 desktop=false，确认旧进程结束及原生恢复标记后才唤醒。严格用例随后暴露 Chromium 的 Crashed 启动会阻断原生恢复；仅在受管休眠恢复标记存在时增加原生启动开关，未另存页面或重建资料。另修正最后窗口先断开CDP造成关闭误报：只发送一次关闭命令，以目标消失或浏览器端口退出确认完成，并连续三次验收。最终两套完整功能验收均使用修正后的helper和严格夹具；此前资源阻塞、失败、部分运行及旧夹具结果均不算最终通过。

功能验收后仅修正两处引导文字：action_timeout_ms明确为元素交互等待，超出视口的拖动提示改为使用ref或分步移动，不再建议获取不存在的视口截图。逐字节核对确认仅两个字符串改变，参数、默认值、校验及执行逻辑均未变；两种feature的真实MCP目录各5项合同检查、最终helper8项及构建通过。说明收尾源码SHA-256为 `ad47aed46b81030474fb5e11d7a34cbab17e005172b9e11a0ddb80b87e84ccd8`；最终目录和程序摘要见[说明收尾核对](evidence/runcove-structured-browser-20261020/wording-review.json)。上述完整计时报告及输入保留原值，没有将其改写为另一份程序的报告。

随后按用户确认的模型可见文本落实四个工具、22个动作分支及全部字段描述；真实HTTP MCP `tools/list` 中103条描述与确认稿逐项匹配，Server与Standalone目录一致。`browser_act.actions.items` 引用 `$defs.ActStep`，动作与字段说明完整包含在22个 `oneOf` 分支中。`wait_secs` 的运行时默认值、schema default和上限统一为30秒，范围0..30；参数校验及任务owner同步调整。两种feature各30项MCP单元、5项真实协议合同检查通过；任务owner经过真实30秒等待验证，覆盖等待结束保留运行任务、21秒后完成及取消通知。全workspace/all-targets Clippy、Standalone库/独立入口/合同测试Clippy及无MSB独立入口构建通过。初次Standalone all-targets检查误包含依赖MSB的VM测试，改用独立入口适用范围后通过，未修改VM测试。除说明/schema和等待默认值、上限、范围错误文字外，执行逻辑不变；该针对性验证阶段未重跑完整浏览器VM资格，也未更新生产或发布镜像。见[确认后的实际工具目录](evidence/runcove-structured-browser-20261020/browser-tools-approved.json)及[针对性验证](evidence/runcove-structured-browser-20261020/approved-contract-verification.json)。

上述完整功能资格使用固定开发资产，两份运行报告均为 development assets、shared_full=true、full=false；不代替Server完整发布生命周期、外部Agent/模型/网关及第三方网站联调。本轮未构建最终发布镜像或离线包，新改动尚未提交或推送。

随后按用户要求快速更新生产，未备份、未迁移。以源码摘要 `1d5d3632fa34cf691caba6d86b162f8df08748bba5dcfe9059abff0f63762594` 构建静态链接SQLite 3.51.3的release程序，在原容器内同时替换MCP及内嵌浏览器worker的desktop helper，同步release/runtime元数据并重启；运行进程、程序、helper及元数据摘要一致，原容器、基础镜像、配置、挂载和端口核对通过。真实HTTP工具目录与确认稿完全一致，临时空间通过bash/文件写入、快照/ref点击/读取、iframe填写、整页及桌面图片验证；32秒间隔截图任务在省略wait_secs后等待24.81秒成功返回，验证30秒默认等待路径。临时空间和上传暂存已清理，原账号数量与重启前一致，功能验收前后原账号记录未变；生产及相邻服务健康。首次核对因Docker挂载数组顺序变化中断，按挂载目标排序后内容一致，续跑验收通过，没有再次部署。此更新仅在容器可写层生效，重建容器需重新应用或构建新镜像。具体程序摘要与现场范围见[生产更新验证](evidence/runcove-structured-browser-20261020/production-update.json)。

## bash 参数与生产二进制更新（2026-10-19）

共享 MCP 的 bash 命令字符串参数从 `command` 改为必填 `cmd`，同步现行调用、参数合同和接入文档；固定旧镜像的验证仍使用其原参数。

构建输入 SHA-256 为 `b3dfbf34d7ca75cb58b91fe37cd1b56280e9f3b8412ad4cdb694c6256d54df37`，release MCP 程序 SHA-256 为 `97994ec5976bab0443c71e7e7b94d4dac30e6850c58780f545f29fc92a146aea`，静态链接 SQLite 3.51.3。MCP 会话3项、工具合同5项、验证器协议8项回归及格式、Python语法检查通过。原生产容器内替换程序、同步 `release.json` 并重启，已核对运行进程摘要、工具目录必填 `cmd` 和真实 Bash 执行；服务健康，原挂载目标、端口和现有账号核对通过，临时验收账号与传输文件已清理。

按用户要求未备份、未迁移；沿用镜像 `sha256:dc5d61e08fa29c02775089083efe31ae2968ad6246d317600d5a52e90cb4b861`，本次覆盖只存在于容器可写层，重建容器需重新应用。本轮未重建镜像或离线包，未执行完整发布资格。验证与中间失败说明见[针对性验证摘要](evidence/runcove-bash-cmd-20261019/verification.json)。

## 人工接管刷新空闲时间（2026-10-19）

成功接管通过现有人工输入入口刷新一次桌面和空间工作活动，从接管成功时重新计算空闲；60秒无输入交还AI、300秒桌面空闲规则不变。失败接管、主动交还及状态轮询不刷新，不新增计时器、配置或持久化。

构建输入 SHA-256 为 `61a1d41f93a18d9bc347484a97380fd714bee44f741db9b18b9f2df53be432cf`，Server 镜像 `runcove-server:runcove-61a1d41f93a18d9b`，ID `sha256:dc5d61e08fa29c02775089083efe31ae2968ad6246d317600d5a52e90cb4b861`。两种 feature 方式各3项路由回归、5项控制权回归、Clippy及构建内177项MSB回归通过；独立新空间验证真实浏览器、接管、第二观看者拒绝和交还后继续工作。随后更新生产镜像并通过公网认证控制路由验收，临时账号已删除；原账号、配置、挂载和其他服务保留，SQLite完整性正常、外键违规0，启动错误和自动重启0。未备份或迁移；按用户授权清理本机及生产的旧备份、迁移暂存、历史镜像与本轮传输文件，保留当前配置路径和业务数据。未重跑真实五分钟计时、完整发布/大文件压力或构建新Standalone镜像。见[针对性验证摘要](evidence/runcove-browser-pages-20261019/takeover-activity.json)。

## 浏览器 Page 上限与资料恢复（2026-10-19）

Server、Standalone 共用可配置的 `[browser].max_pages`，默认4；desktop helper 统一限制标签页、新窗口和网页弹窗，确认关闭后才释放数量。满额通过原有错误结构返回 `PAGE_LIMIT_REACHED`，正常结果不新增用量字段；工作台增加“打开浏览器”，人工接管关闭 Chromium 后仍可重开且保持控制权。个人资料选择器不占 Page、不被自动补空白页绕过；受管闲置休眠完成后写入 Chromium 原生的一次性恢复标记，唤醒继续最后选择和全部资料页面，主动关闭后新开仍遵循浏览器设置。合同与研究来源见[浏览器说明](browser-desktop.md#个人资料选择与休眠恢复)。

首轮开发构建输入 SHA-256 为 `831d43683fda0e561e4bf02726fda67c3de44a4eabcc6ef380637c78eabce9f5`。Rust 单元128项、MCP合同5项、受管清理8项、执行所有权5项、打包5项、验证器HTTP协议8项通过；全 workspace/全目标及无默认 feature Standalone 的 Clippy、独立入口构建、UI构建/i18n/DOM合同通过。具体程序与测试摘要见[验证摘要](evidence/runcove-browser-pages-20261019/verification.json)。

独立 Server 以省略配置的默认4页、Standalone 以配置覆盖的2页验收：Server 浏览器14项，Standalone 13项通过、1项 Server 专属测试跳过；两者均通过 Page上限5项、媒体9项、双资料选择与真实300秒恢复1项，以及实际VNC重开/被动连接真实300秒休眠2项。资料测试验证持久恢复标记、唤醒后第一条脚本直接恢复全部页面与最后资料、localStorage保留、原选择器设置不变，以及主动关闭后重新出现选择器；使用本地资料与HTTP夹具，未登录真实Google账号。再重启并将上限分别从4或2降低至1，已有页面全部保留，新增被拒绝，已有页可导航及关闭；Standalone 原身份隔离、控制器故障阻断、SSE关闭和重启持久化检查通过。

该轮两套报告均为 `development assets`、`full=false`，源码、程序与测试输入固定，容器和数据卷清理通过。该轮没有重建发布镜像/离线包，没有部署生产，也没有执行全套文件流或VM生命周期发布资格；这些实测不能替代最终镜像资格。早期一次首个 Server 观察未得到预期健康字段，原错误未留存，原因不能确定；测试现保留原始返回，另做3次独立冷启动及完整浏览器回归通过。其余中间运行因调整恢复实现而中断，或因资料测试夹具附带URL额外新增页面被额度正确拒绝而重跑，均不作为最终成功报告。

后续完整复审修正了脚本快速结束时漏报弹窗超额、经选择器延后恢复时的旧页面基线及快照顺序、最后一页关闭后的结果丢失，以及闲置回收依赖 CDP 响应的问题。收尾先保存结果，再尽力恢复下载设置；取消保持原清理时限，闲置关闭直接使用同一 Page owner 已有状态。负例夹具改为明确刷新输出后退出。正式构建另发现 `.dockerignore` 排除了必需运行时缓存，已仅放行经过校验的公开输入，并将过滤规则纳入源码摘要。

最终构建输入 SHA-256 为 `395b6c2c66ada27d3f0cfce93a3d2d01aeb5941d8322475b785db7abb9d0c47a`，Git 构建基线 `5b726c6`，实际未提交输入由该摘要识别。Server 镜像为 `runcove-server:runcove-395b6c2c66ada27d`，ID `sha256:e7f831ff60b651ad1a1a869b52c18de521169b311ce672940bc232b19077bb63`；Standalone 同版本 ID 为 `sha256:a77343bd235610cf2ad2474df35917cddb6fa393ced093db72644044e3363f29`。138项 workspace 单元/二进制测试、5项MCP合同、两种构建方式的 Clippy、26项Python回归和构建内177项MSB回归通过；14份实际测试/夹具输入冻结。见[最终镜像与生产验证](evidence/runcove-browser-pages-20261019/release-verification.json)。

两目标均使用最终镜像、`full=true`，全部隔离资源清理成功：Server 浏览器14项，Standalone 13项通过及1项Server专属跳过；两者的Page上限6项、媒体9项、文件流3项、多资料恢复1项、闲置回收1项和真实UI5项均通过。资料恢复、闲置及被动VNC分别经历真实300秒；闲置回归在临近关闭时暂停 Chromium 主进程，仍能回收并恢复原页面和 localStorage。Server另通过310秒MCP会话及完整生命周期验收；从当前生产基础镜像升级的独立演练保留旧Workspace、资料、文件和凭据。上限分别由默认4、配置2降为1，经选择器恢复的旧页面全部保留，新增被拒绝，导航和关闭继续可用。

随后按用户授权直接替换生产Server镜像，未创建备份、未执行数据库迁移，state schema仍为3；持久化目录布局不变，本期新增的持久化行为为浏览器原生恢复意图。原配置省略 `[browser]`，实际采用默认4。原有两个账号、凭据、挂载、端口、代理配置及其他服务保留。临时账号的九工具、实际网页及MCP图片、四页和弹窗错误、HUMAN重开、DAV Range与认证发布全部通过，账号及资源正常删除；只读SQLite完整性正常、外键违规0、原sandbox/volume数量保留，程序/UI摘要匹配release元数据，本次启动运行时错误0、自动重启0。

首次部署在健康启动后，因进程核对命令缺少PID列被Docker拒绝，脚本自动恢复原容器；修正为 `ps -eo pid,comm` 后重新部署成功。首次现场验收又遇到测试封装参数名与发布工具参数冲突，临时账号已清理；修正后完整验收通过。这些是部署/验收脚本问题，实际过程保留在最终验证摘要中。部分中间镜像因后续复审修正而停止资格执行，不作为最终通过结果。本轮没有制作新的离线包，也未重跑完整4 GiB DAV压力、真实写满或外部Agent/模型/NAS/ARM联调；不沿用历史报告替代这些范围。

## Server 桌面身份统一（2026-10-17）

Server 的工具、Chromium、桌面应用和内部观察统一使用 VM 内 root、HOME=/root；Chromium 增加 `--no-sandbox`，空间隔离依赖 VM。Standalone 继续由 root 控制进程管理 UID/GID1001 用户执行及原浏览器沙箱。沿用同一身份选择，不增加配置或路径白名单；仅准备固定受管目录，不递归改写工作区归属，继续使用既有 profile。保留工作区已有九工具及参数说明整理，补充绝对路径、原生上传对话框的 Ctrl+L→输入路径→新截图点击打开，以及实际上传确认。

源码检查通过 Rust 单元124项、MCP 合同5项、受管清理8项、执行所有权5项及打包5项；全 workspace/全目标 Clippy、Standalone 无默认 feature 的 lib/bin Clippy、无 MSB Standalone 构建和格式检查通过。发布 MCP 按固定静态库重新构建，链接符号验证 SQLite 3.51.3。最终构建输入 SHA-256 为 `04da2346492e085c299b7a74f4cd43afe28ccb996c23302c93d2158ddb29b9e6`，具体输入、二进制和独立运行结果见[验证摘要](evidence/runcove-identity-20261017/verification.json)。

固定输入的独立真实 Server 通过浏览器14项、媒体9项：0700目录内0600文件的中文/空格绝对路径经真实 GTK 选择器上传，接收端确认完整1 MiB及SHA-256；另验证 setInputFiles、filechooser、自动化下载、普通桌面下载及脚本失败后的普通下载。进程核对 Chromium、Xvnc、LXQt和脚本均为root。独立旧版helper实际以UID1001创建profile，再使用同一卷重启为新版root，标签、Cookie和localStorage保留；没有导入或改写业务数据。

Standalone 在另一独立开发资产容器中通过浏览器13项、媒体9项；1项Server专属profile重开测试明确跳过。用户执行仍为UID/GID1001，秘密/私有路径/符号链接边界、文件互通、控制进程丢失、重启后的工作卷保留和登录失效通过。所有资格容器及卷已清理。输入一致性、一次profile测试预期及本机Docker Desktop附加验证脚本的修正单独记录，早期结果不当作最终资格。

本轮为开发资产与针对性真实运行验证，full=false；未重建发布镜像或离线包，未执行长时间休眠、完整UI或第三方网站/模型联调。

实现经审查提交为 `4551dcb` 后，直接原子替换生产容器的 MCP、desktop helper 和对应 release 元数据并重启，未备份或执行迁移。生产临时隔离账号完成九工具及4项浏览器补充用例，确认原生上传的1 MiB接收哈希、两种下载、root进程身份，以及工作台本机上传→`/workspace/<文件名>`→网站接收的完整字节和哈希。首次补充测试因脚本目录错误未启动，修正后完整复验通过；两次临时账号均由正常删除流程清理。原有两个账号记录、配置、挂载、运行时和其他服务保留，容器健康，本次启动运行时错误为0，上传的二进制暂存文件已移除。详见验证摘要的 production_binary_update。

这仍是原镜像 `sha256:413c3411b962e140b89f9f1a7195b4350ff92af2f76f3dec5a9fe3a00c129260` 上的容器可写层覆盖，没有创建新镜像；按原镜像重建容器需重新应用二进制。本次现场调用不延伸为完整镜像资格或第三方服务联调结论。

## 九工具说明整理（2026-10-17）

核对命令、文件、浏览器、截图、任务和发布实现后，统一整理两组共9个工具及33个参数的说明。以当前连接的 Runcove 空间明确操作域，在工具描述或关键参数中交代归属，区分文件、标签页、截图帧、任务和发布项；保留正常调用所需的接口、事件顺序及输出边界，精简重复、低概率异常提醒和无关部署细节，异常原因由调用错误返回说明。静态比对确认参数结构、默认值、校验与执行逻辑未变，同步术语参考；仅做静态差异及格式检查，按用户要求未编译或运行测试，未更新运行服务。

## 执行身份与依赖安装说明（2026-10-17）

保留现有权限架构：Standalone 的服务控制进程使用 root，Agent 命令、浏览器及文件操作使用 UID/GID1001；系统依赖由镜像提供，项目依赖可由 Agent 安装。Server 的命令和文件操作继续使用 VM 内 root，核心服务位于 VM 外。两种编排统一工具行为，执行身份按各自的隔离边界确定，详见[权限合同](architecture.md#执行身份与环境管理)。

本轮同步产品首页、开发约定、架构、部署、浏览器合同及 guest 使用说明，并合并两处工具描述的措辞调整，参数、默认值与权限实现不变。检查本地文档链接及锚点、离线手册复制、5项打包回归、5项 MCP 工具合同和格式检查；未构建发布二进制、镜像或更新生产，既有固定验证记录保持原输入与范围。

## 浏览器与图片工具调整（2026-10-17）

基线 `61dbf67` 加本轮实现，最终构建输入 SHA-256 为 `0c48b4974e55c4ae8a87a9626dbd95aeea27bcee6c5a618ce344428f1acb33b4`。两个入口继续共用工具、任务和文件执行链路。`browser_observe` 分开 DOM/AX，`browser_run` 只返回显式输出，`browser_task` 同时管理截图序列；新增 `screen_observe`，默认单张、最多8张，可选间隔及原始 PNG 保存目录，返回各帧和已保存路径。`read` 增加默认关闭的 multimodal 图片读取，Rust 统一有界处理；工具结果预算为12 MiB，默认文本输出上限保持不变。合同见[浏览器与图片工具](browser-desktop.md#浏览器与截图工具)。

完整 Rust workspace 串行 **315 passed / 0 failed / 0 ignored**，最终受影响模块另通过 MCP 27 项、desktop helper 6 项；两种构建的 Clippy、Server/无 MSB Standalone 构建、无 MSB 依赖检查和 UI 构建通过。Python 清理8项、执行所有权5项、MSB补丁4项、协议8项、DAV验证器3项、打包5项通过；独立 root 容器中的工作区导入9项通过。检查和二进制摘要见[验证摘要](evidence/runcove-tools-20261017/verification.json)。

按用户要求收缩为快速验收，只使用一个新建容器及卷，绑定当前开发二进制：真实 MCP 浏览器11项、媒体8项、文件流3项通过，另通过短流程启动、权限、退出和重启检查。媒体覆盖四格式、方向和透明度、GIF/APNG/WebP首帧、接近10 MiB源文件、超过50 KiB的完整图片、多 MiB工具结果、1..8帧和间隔、页面关闭、Chromium卡住时的桌面观察、原图紧凑命名及路径、认证HTTP下载、保存权限失败、并发和取消保留输出。

首轮长流程在上述三个套件通过后按用户要求停止，300秒休眠及真实UI套件未完成，未启动第二个部署验收；隔离容器与卷已清理。该轮仅做源码及开发资产验证，没有重建发布镜像、部署生产、推送镜像或完成外部Agent/模型/网关的多模态联调，不能作为最终镜像资格报告。

随后对全部九个工具的参数、说明、返回、预算及执行链路复审，修正 BM25/II* 普通文本误判、很小文本限额截断文件协议及图片错误、旧截图视图忽略显式任务历史；补齐有界短读循环、参数提前校验、保存错误中的目录信息，并移除重复错误码解析。复审源码输入 SHA-256 为 `16d3e95d65205e0389883f53ed603e7cd470da52d1b940c83a468ba76366851a`。8字节输出限额的真实 VM/MCP 回归、文件分类与短读3项、MCP单元/合同32项、helper 6项、清理8项、打包5项及两种构建的 Clippy 通过。新增 `--tools-only` 快速验收模式，单个隔离开发资产容器的浏览器11项、媒体8项及基础重启检查通过并清理；报告 full=false，不绕过发布资格。详细检查摘要追加在同一[验证摘要](evidence/runcove-tools-20261017/verification.json)的 review 字段。

当时对实际 Server 调用中出现的自定义截图目录权限失败仅补充了参数说明，构建输入为 `83fe72521872f94f191aa99b2c10a6131491ca59769a18b832466e1b2f4f873b`，5项工具合同回归通过，发布二进制重新构建并验证实际链接 SQLite 3.51.3。后续权限复审确认这是工具执行身份不一致，已按下述规则修正，不能将当时的限制说明作为当前合同。

复审及说明修正分别提交为 `abda7df`、`b083dc4` 后，按用户要求直接更新现有生产 Server 容器，未创建备份或执行迁移。只原子替换 MCP、desktop helper 及对应 release 元数据并重启；服务二进制 SHA-256 为 `623dc2a6198fcc3e48ad4e7e74f7ed5be6a4ea3231349a8672741106d0300196`，desktop helper 为 `75f3d3c182a6a579f53e0e442f0d68ed515bc96f197260385ab9d43420854292`。原镜像 ID、配置、挂载、运行时和原有两个账号记录保持不变，其他服务未重启；容器健康，本次启动运行时错误为0。用临时隔离账号实际调用九个工具，核对超过50 KiB完整图片、DOM/AX、显式脚本结果及任务查询、连续截图、原图文件路径和认证下载哈希；账号及其资源由正常删除流程清理，上传二进制临时文件已移除。结果见验证摘要的 production_binary_update 字段。

这仍是原镜像上的容器可写层覆盖，没有重建镜像或离线包；由原镜像重新创建容器不会包含本次程序更新，需重新应用二进制。现场调用验证不延伸为完整镜像资格或外部Agent/模型/网关的多模态联调结论。

随后以 `47b8d58` 为基线复审全部九个工具的权限链路，修正截图保存、`browser_run` 用户脚本和产物枚举固定使用桌面身份的问题。它们现在与空间命令和文件工具一致：Server 使用 VM 内 root，Standalone 保持 UID1001；任务目录及结果归属检查同步匹配。Chromium、桌面及内部观察进程继续使用 UID1001，无新增配置、权限参数或目录白名单。构建输入 SHA-256 为 `4a2fa14a42ed8441bb75c4367d474de74c209428103943a2a69fdbec0d75eaca`，工具说明、工作区指引和合同文档同步更新。

权限复审的快速验证通过 helper 6项、MCP 32项、清理8项、执行所有权5项、打包5项、全目标及无 MSB Standalone Clippy。隔离进程的8项检查复现旧版拒绝并验证 Server 私有目录保存、产物枚举、脚本身份及私有文件上传，Standalone 仍拒绝 root 私有路径。只启动一个 Standalone 验收部署，浏览器11项、媒体9项、秘密隔离和基础重启通过，full=false，容器与卷已清理；新增共同回归覆盖 bash 创建0700目录后的文件读写、浏览器脚本、页面/桌面原图、认证下载和产物列表。发布程序重新构建，SQLite 实际链接版本为3.51.3。首轮开发资产重启受测试中重编译绑定文件干扰，固定输入后复验通过。详情见验证摘要的 workspace_permissions_fix 字段。

权限修复提交 `3f75721` 后，再次直接更新现有生产容器的 MCP 和 desktop helper 并重启，未备份或迁移。生产程序 SHA-256 分别为 `8f03681ff762b35373f8449bb160dc47f3e513ba8fd8858717d249ce4d0a40d7`、`c207dba08b01331faef84c099e9e8d7d27512fe44da4b49e41ec6c7740ad133c`。临时账号的九工具调用通过，实际验证 root 创建的0700目录中脚本读写及上传、页面/桌面原图保存、MCP图片读取、认证下载哈希和产物枚举；原账号、配置、挂载、运行时及其他服务保持不变，服务健康，本次启动运行时错误为0，临时账号已正常清理。首次原始挂载列表相等断言未通过，未留存当次列表；之后按目标路径核对全部挂载字段、端口及完整复验通过，不能据此确定首次断言失败原因。此次仍为原镜像上的二进制覆盖，重新创建容器需重新应用；详情见 workspace_permissions_fix.production_binary_update。

## 工具描述与生产二进制更新（2026-10-16）

提交 `7e3f206` 统一八个 MCP 工具的远程工作区归属说明：`bash`、`browser_observe` 保留工具描述，`read`、`write`、`edit` 的 `path`、`browser_run` 的 `code` 和 `browser_task` 的 `task_id` 补充归属，`web_publish` 在工具描述首句补充。每个工具的描述与参数描述合计出现一次 `Runcove's remote workspace`；未改执行逻辑或参数结构。`cargo fmt --check` 及无 MSB 构建下的 `test_web_publish_mcp` 五项测试通过。

以锁定依赖和 SQLite 3.51.3 静态库构建 release MCP 程序，在原生产容器内原子替换二进制并执行重启，沿用原运行时、helper、UI、配置和数据卷。构建输入 SHA-256 为 `3c7f88090ecc2b981be67270927f0997da60a8777302ea13b504fb4485075fc6`，部署二进制 SHA-256 为 `aa9db24889feb603e5c191216084400b6e1de7e9dae303bbc1ad442faaff8107`；容器内 `release.json` 的程序摘要与覆盖来源已同步更新。

本次是原镜像 `sha256:413c3411b962e140b89f9f1a7195b4350ff92af2f76f3dec5a9fe3a00c129260` 上的容器可写层覆盖，未重建镜像或离线包；重新创建容器不会自动包含本次覆盖。未备份、未执行生产功能验收，既有镜像资格不延伸至本次二进制更新。构建与部署临时文件已清理。

## 上游介绍与资格脚本审查（2026-10-16）

基线 `459c385` 加本次变更：在[仓库首页](../README.md#microsandbox-与项目定位)补充 microsandbox 介绍、致谢及三个固定上游版本的定位对照；比较来自官方/项目自身文档，没有执行跨项目运行或性能测试。文档数量保持不变。

审查现有生产验收改动，确认 DAV 自建临时容器需同时清理匿名日志卷；修复桌面验收脚本结束后未恢复调用方管理员环境变量的问题。无 Docker 的回归覆盖失败诊断中的三类测试凭据脱敏、环境恢复、清理失败记录及匿名卷回收：`docker/test_package.py` 5 项、`docker/test_verify_dav_large.py` 3 项通过。当时另核对生产材料的镜像/来源关联及报告引用；后续公开化仅保留必要的脱敏结果投影。20 轮唤醒摘要已与本机原始报告及逐轮结果核对，投影规则写入 `wake-cycles.json`。

本次仅修改文档、验收驱动和回归测试；未改服务/guest 实现，没有重建镜像或重跑真实 VM/桌面资格。下节生产结果属于其记录的既有镜像与执行日期，不是本次回归重新取得的结果。

## 生产 Server 升级（2026-10-16）

历史部署镜像为 **Runcove Server `runcove-fc010ed2726e9bec`**，image ID **`sha256:413c3411b962e140b89f9f1a7195b4350ff92af2f76f3dec5a9fe3a00c129260`**；Git 构建基线 `d5040e2`，实际构建输入 SHA-256 为 `fc010ed2726e9bec25c3872caeb69aba65078db53e8f3518526becca06380811`。这是固定历史结果，不表示当前源码重新构建或当前服务状态。见[公开结果摘要](evidence/runcove-production-20261016/verification.json)。

- 同一最终镜像通过生命周期、完整桌面、精确旧生产版本升级/独立卷冷备回滚、4 GiB DAV 和真实写满资格；构建内 177 项 MSB 回归通过。真实 UI 四项端到端、两段实际五分钟休眠/恢复、1 GiB 文件、4 GiB 逐字节读取、80 次取消、20 次写满通过。见[资格](evidence/runcove-production-20261016/qualification.json)、[UI 日志](evidence/runcove-production-20261016/server-ui.log)和[升级回滚](evidence/runcove-production-20261016/upgrade-rollback.json)。
- 现场升级记录核对过原账号、凭据、固定镜像及工作盘保留，后续生产功能与重启验证完成。公开树仅保留验证结论和发布产物身份；主机、域名、账号/空间标识、配置、磁盘清单、备份位置和真实部署截图不公开。
- 首次休眠后观察缺少 `health` 字段，原测试未保留工具原始响应，原因未定。最终完整资格及 20 轮关闭/唤醒通过，不能据此声称首次异常已修复。见[首个失败](evidence/runcove-production-20261016/initial-wake-failure.log)及[唤醒结果](evidence/runcove-production-20261016/wake-cycles.json)。

## 源码审查与维护整理（2026-10-16）

### 双版本与拆分前实现的对照审查

基线是规划提交 `a8f50c53618388115aa671a7072ac4151c58b158` 的父提交 `a8629c1`，审查起点为 `4a88cfc`。按产品边界、共享核心、Server 行为保留、Standalone 适配、认证/路由、配置转换、状态不变量、进程/helper、浏览器/文件、镜像及交付/迁移逐项对照，详细落点与复验见[对照报告](evidence/runcove-review-20261016/comparison.json)。

结论：共享核心和两个运行适配的划分符合产品目标。Server 保留管理 API、原空间身份、凭据与 VM 生命周期；Standalone 复用工具和工作台，固定一个空间，不包含 MSB、管理 API 或持久用户清单。两者还存在隔离方式、资源/生命周期配置和持久化内容的必要差异，不能将区别概括成只去掉管理 API。Workspace 为 VM rootfs，不在其中嵌套 MSB。首版平台仍是 Linux/amd64，NAS/ARM 需分别验收。

本次新增修复：

- 资格脚本检查 ID 后仍以可变标签启动，可能让报告与实际被测镜像不一致；现在初始化、运行和旧版回滚均固定已检查的 ID，打包器也按报告 ID 选择镜像。模拟标签在检查后变化，修复前 5 个场景失败；修复后覆盖 7 个验证入口及打包选择。
- 工作区导入只校验原归档，预览遗漏旧暂存重定位新增的父目录冲突；现在预览与 apply 都在创建 stage 前校验。原实现实际提取会因冲突失败，未复现越界写入。
- Standalone 的 DAV origin 仅含空白时，配置校验允许留空但装配仍尝试解析 URL，导致启动失败；现在与校验使用相同空值语义，并纳入完整 Standalone 资格输入。

最终完整 Rust 复验 **307 passed / 0 failed / 0 ignored**，两种构建的 Clippy、无 MSB 依赖检查、Python 回归和 UI 静态检查通过；独立 root 容器的 9 项导入测试、7 个验证入口及打包选择的标签变化回归通过。真实 Standalone 容器验证空白 DAV origin 修复前退出、修复后监听正常，容器与卷已清理。

初次完整 Rust 回归的 Server DAV VM 测试在连续 HEAD 的第 75 次返回 503，日志显示运行时准入的 `Permission denied`；单独复测及随后完整复验通过。原因尚未定位，首次失败与后续复验分别留存于对照报告，不能从后续成功推断已修复该异常。

该轮审查仅提交源码与证据，未重建发布镜像、生成离线包、修改当时的生产环境或推送 registry；下列旧镜像记录保持原身份和日期。

### 前次审查修复与验证范围

本轮复核共享核心、两个入口/运行适配、认证与代理、状态/资源、进程/helper、浏览器/文件、构建/验证/打包及文档关系，修复：

- 新格式配置转换误用旧监听/容量默认值；转换与正常加载改为同一解析入口。
- 本地执行 owner 提前关闭输出后，进程等待遗漏取消和超时回收；输出 EOF 与进程退出继续由同一 monitor 持有。
- 接管请求被取消后遗留 TRANSITION；释放交接状态，但原操作仍占位，清理失败继续 BLOCKED。
- root 桌面 helper 经用户产物目录链接枚举私有文件元信息；完整遍历下放 UID1001 子进程。
- VM CPU 从 u32 窄化到 u8 可截断，非法配额未在启动前拒绝；配置校验并在创建任何磁盘前检查转换，零内存/盘和输出限额一并报错。

产物目录问题在上一版精确镜像上复现为 HTTP 200 并返回 root 私有文件名；修复后的开发资产容器回归要求拒绝且不泄露元信息，并验证正常产物枚举恢复。前述配置、执行和接管问题也有失败后通过的回归。

源码、运行二进制和测试范围见[本轮审查报告](evidence/runcove-review-20261016/verification.json)。完整 Rust/Clippy、无 MSB 构建、进程/迁移/打包回归与独立容器桌面场景分别记录。已完成计划、旧文档索引及过程性发布流水已移除；有效的产品边界、回归矩阵和操作步骤归入仓库首页、架构、开发与部署/迁移指南。

**该轮审查没有重建发布镜像或重新生成离线包。** 下节 2026-10-15 镜像及离线包不含这些修复，保留为历史资格证据，不能当作当前源码的发布结果；后续发布须使用新源码摘要，重新通过相应镜像门槛。该轮未修改当时的生产环境。

## Runcove Server / Standalone 分离验收（2026-10-15）

同一仓库共享 MCP、基础工具、浏览器、工作台、控制权、文件和发布核心。Server 保持 Docker + MSB VM 多空间与管理 API；Standalone 为一个使用者、一个空间的 Docker 容器，不带 MSB/KVM、管理 API 或静态多用户模式。Workspace 是 Server 使用的空间 rootfs，不在空间中嵌套 MSB。配置按目标收紧职责和默认值，旧配置显式转换并保留有效值；Server 状态仍为 schema3，强化身份/资源不变量，Standalone 不建立持久账号库。

当时应用版本 **`runcove-2e50d622ab9e4573`**，构建输入 SHA-256 **`2e50d622ab9e45736a9d4816831eede4656ba9d8783b2a34c15d8a42c7081532`**；构建时 Git 基线为规划提交 `a8f50c5` 加本轮实现，不能把该 revision 单独当成最终源码身份。

| 目标 | 最终镜像 ID | 资格范围 |
|---|---|---|
| Server | `sha256:e5913326b6f3bac140e4524ffd8468bb0d77aac353c674430308877481e94105` | 生命周期/账号/发布、真实 VM 桌面、1 GiB 文件、4 GiB DAV、磁盘写满、旧生产升级回滚 |
| Standalone | `sha256:b13c172f4e8a21c42d256dbbea21ecb6fcd4a578847851c85e98fd47a25c2993` | 无 KVM 启动、单空间/秘密/UID 隔离、真实桌面、1 GiB 文件、4 GiB DAV、rclone/FUSE、磁盘写满、卷恢复 |

- 源码：当前三个 helper 先构建，完整 workspace 串行 **303 passed / 0 failed / 0 ignored**；全目标 Clippy `-D warnings`、fmt、Standalone 无默认 feature 编译及 normal/build 依赖图通过。依赖图排除 MSB/libkrun；受管清理 6 项、执行所有权 5 项、文件迁移 8 项和协议/DAV/打包回归通过。详见[源码检查](evidence/runcove-split-20261015/code-validation.json)。
- 两目标使用同一组真实浏览器、文件和生命周期套件；各有 **4 项 Playwright 端到端通过**，包括真实 RFB 键鼠/剪贴板、双观看者、人工接管不阻塞基础工具、实际超过 300 秒被动观看休眠/标签恢复，以及 HTTPS + 改写 Host + 嵌套前缀。Standalone 另验证丢失桌面 owner 后 BLOCKED、控制进程死亡后数据子进程收敛、重启后旧登录失效和工作卷保留。报告：[Server 桌面](evidence/runcove-split-20261015/server-desktop.json)、[生命周期](evidence/runcove-split-20261015/server-lifecycle.json)、[Standalone](evidence/runcove-split-20261015/standalone.json)。
- 两目标完整读取 **4,294,967,296 bytes** 并逐字节核对，验证跨 2 GiB/尾部范围、真实截短、旧 ETag、80 次取消和后续可用；各 **20 次真实 8 MiB 文件系统 ENOSPC** 返回 507、保留原文件且可继续写入。拒绝 PUT 后即使每 40 ms 继续发数据，连接仍在同一个约 2 秒截止收敛。报告：[Server 文件](evidence/runcove-split-20261015/server-files.json)/[写满](evidence/runcove-split-20261015/server-storage.json)、[Standalone 文件](evidence/runcove-split-20261015/standalone-files.json)/[写满](evidence/runcove-split-20261015/standalone-storage.json)。Standalone 还通过真实 [rclone/FUSE](evidence/runcove-split-20261015/standalone-rclone.json) 挂载读写；FUSE 权限仅授予独立测试客户端。
- 在新建隔离卷中，用精确规整前生产镜像 `sha256:9ee5cd5fd3998d5defc005b90b1ace906f48484f8fbb680a50a0e6ab7e76ce06` 创建真实 VM，再切换最终 Server，原凭据、空间/盘身份、文件/profile、桌面/DAV/发布和重启恢复通过；完整升级前冷备恢复到另一新卷后旧版回滚通过。相同旧镜像选定工作区导出到最终 Standalone，246 条目、所有者映射、原源未改、标签/profile/链接和用户可写性通过。报告：[升级/回滚](evidence/runcove-split-20261015/production-upgrade.json)、[单空间导入](evidence/runcove-split-20261015/workspace-import.json)。资格容器和卷均完成清理。
- 实际模型经正式 HTTP MCP 完成“观察→CSV→浏览器上传→下载→MCP 摘要→工作台下载”链路，65,536 bytes 摘要一致；真实连接的中英文 320/390/1440 六种布局已查看，无溢出或页面错误。这是较早候选 `37a033…` 上、未变的共同 MCP/UI 组件证据，不能标成最终镜像运行记录。见[模型与布局](evidence/runcove-split-20261015/model-workflow.json)。
- 较早一次完整 UI 运行出现休眠后恢复为新标签页，根因尚未确定。后续最终镜像完整套件均通过，另有 [20 次真实浏览器关闭/重启恢复](evidence/runcove-split-20261015/desktop-restore-cycles.jsonl) 通过；这些结果不等于已经定位或证明修复最初偶发失败。

离线包的加载、Compose 和摘要以[离线包检查](evidence/runcove-split-20261015/packages.json)为准；[总证据](evidence/runcove-split-20261015/verification.json)记录最终身份。子代理只读审查发现的问题已修复，见[实现审查](evidence/runcove-split-20261015/implementation-review.json)。本次资格平台为 Linux/amd64；没有 ARM/目标 NAS 验收、registry 推送或生产切换，升级驱动使用的精确旧镜像见[公开镜像基线](evidence/runcove-split-20261015/production-baseline.json)，账号和拓扑快照不公开。


## 工作区恢复记录（2026-10-14）

历史恢复只导入工作区文件，不导入旧根盘、账号状态或 MSB 数据库，目的空间身份和凭据保留。27,722 个普通文件在离线恢复及重启后逐项校验，源 UID/GID、权限和 66 个符号链接恢复；旧上传暂存归属冲突通过完整留存旧目录、由正常 DAV 创建新归属处理。DAV PROPFIND、抽样 GET/摘要和实际文件入口通过，当时没有完成目的账号的公网桌面登录/控制验收。

公开材料只保留[恢复结果投影](evidence/fresh-production-20261014/restore.json)和[独立镜像资格](evidence/fresh-production-20261014/qualification.json)，不包含业务文件清单、原始账号、部署配置或备份位置。具体维护步骤见[运维手册](operations.md#将完整工作区备份恢复到新空间)。

## MSB 运行时固定证据

| 固定范围 | 保留的结论与证据 |
|---|---|
| 2026-10-13 生命周期补丁基底 | 同源 SDK/CLI、SQLite 3.51.3；177 项构建内回归。见[构建验收](evidence/msb-lifecycle-20261013/verification.json)与[配方](evidence/msb-lifecycle-20261013/Dockerfile.sdk-runtime-cache) |
| 2026-10-13 应用与真实重启 | 293 项串行 Rust、12 项隔离恢复、原盘/凭据保留。见[复核](evidence/msb-lifecycle-review-20261013/verification.json)、[恢复资格](evidence/msb-lifecycle-review-20261013/qualification.json) |
| PID 复用原始失败 | 旧版 crashed + 无关 PID 复用时首次 DAV 超时；原现场最初崩溃原因仍未知。见[失败基线](evidence/dav-restart-20261013/baseline-pid-reuse.json) |
| 2026-10-11 部分帧取消 | 确定性 partial-frame 回归与当时实际范围见[记录](evidence/dav-frame-read-20261011/verification.json) |
| SQLite 锁/WAL 机制与旧版恢复 | 机制见[运行时维护](runtime.md#sqlite-防御)，操作见[故障处置](operations.md#sqlite-故障处置)，固定测试输入见[证据目录](evidence/sqlite-20260930/README.md) |

补丁源码、上游链接、历史 relay 复现和逐项撤销条件统一由 [运行时维护](runtime.md)保存。本页不重复维护上游“当前状态”；未被索引的过程性开发/部署流水不随公开源码保留。

## 持续有效的验证边界

- HTTP 健康、VM Running 或单个 MCP 请求不能证明桌面、DAV、外部平台或恢复数据已验收。
- 工作区转移与整套服务回滚是不同操作；只有全套一致冷备能恢复服务状态和磁盘。
- DAV 锁不约束 MCP/bash，目录递归可部分完成，不能承诺 Office 协作或所有网盘客户端兼容。
- 真实最终镜像、开发资产绑定测试、模型操作链路和目标部署现场分别标识；一次成功也不证明所有传输竞态或长期可靠性。
- Linux/amd64 的 Docker 资格不等同于 ARM、目标 NAS、生产切换或 registry 发布。
