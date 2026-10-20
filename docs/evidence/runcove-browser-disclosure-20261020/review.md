# 浏览器工具披露调整审查

- 四工具职责保持集中：snapshot 看结构或显式查 tabs；read 查已知目标、读正文或提取数据；act 批量操作及查询/取消；screen_observe 看无标注页面、桌面及时间序列。
- browser_act 说明连续多步执行、需要新 ref 时结束批次、通用固定等待示例、查询原任务及核对网站结果。
- 顶层任务选择统一 op；22种动作合并为16种。click/drag 使用 ref 或图片坐标；press/scroll 使用 page/desktop 目标；navigate 使用 open/back/forward/reload。没有旧名称兼容路径。
- 先移除已知但无效的步骤字段再检查类型和范围。ref 优先且失效时不回退坐标；非空新动作与任务查询仍冲突；未知字段不自动吞掉。
- 固定延时0..60秒。纯延时沿用现有任务、控制权、取消及期限，不启动浏览器/显示服务、不选择或读取页面。混合批次里的延时也在访问页面前执行。条件等待默认60秒，支持提前满足，不把读取失败当作 gone。
- 页面截图维持当前顶层完整布局；独立滚动区域/iframe 只显示当前区域，不展开虚拟列表/懒加载。长正文引导 read；原生窗口引导 desktop。
- screenshot 单帧忽略 interval；desktop 忽略 page_id；save_original=false/null 忽略 save_dir。
- Schema 删除自动标题、$schema、数值实现格式、无意义 null 默认和内部 ref 格式；保留类型、范围、枚举及有用默认值。
- 保持紧凑输出：正常页面观察不带 tabs、健康字段、内部 phase/worker 日志和空结果。tabs 仅显式查询。保留任务身份、失败原因、已完成步骤与必要产物。
- 普通观察、读取及下载源 URL 限1000字符、标题限200字符；显式返回 truncated 标记，不截断已保存文件内容。
- 计时：已接纳任务返回 duration_secs；动作、连续截图、仍运行/失败任务保留秒精度UTC started_at；普通成功快照、读取和单张截图不带 started_at；查询原任务不重置计时。
- 基础工具一并复核：write 禁止空内容；图片读取忽略行范围；发布工具只暴露秒；bash 返回必要计时。目录读取在 fdopen 前核对同一 fd 的类型，恢复 NOT_A_FILE 合同。

## 验证边界

Node 使用固定 Chromium 与生产 worker。真实部署回归使用独立、可丢弃的 Server/Standalone 容器和冻结的当前开发二进制；镜像本身是既有资格基础镜像，不将开发资产回归称作最终发布镜像资格。外部 Google Flow 登录、风控及生成成功不在本轮工具合同验证范围内。最终结果见 verification.json。
