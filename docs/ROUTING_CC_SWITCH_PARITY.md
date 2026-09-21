# Codex 路由与故障转移：CC Switch 对照说明

本说明记录 Codex-X 本轮路由实现与 CC Switch 的对应关系，供维护者检查行为和后续回归使用。
入口为「设置 → 路由与故障转移」。本轮对齐 Codex 的监听、接管、优先队列、请求重试、熔断和
原生官方账号路由；不等于移植 CC Switch 的全部应用适配器和请求转换功能。

参考版本固定为 CC Switch 提交
[`06082e189d65e6d6dbadc35dacdac1ce6c79d89a`](https://github.com/farion1231/cc-switch/tree/06082e189d65e6d6dbadc35dacdac1ce6c79d89a)
（2026-09-15）。以下描述以该版本源码及本项目实现为准，不能用另一版本的说明文案替代代码行为。

## 三层开关

三个状态分别持久化；界面应同时区分「保存的偏好」和「目前实际运行的状态」。

| 设置字段 | 默认值 | 行为 |
| --- | --- | --- |
| `routerEnabled` | `false` | 启动本地监听服务；单独开启不会改写 Codex 配置。 |
| `takeoverEnabled` | `false` | 将当前 Codex 供应商的传输配置指向本地监听；要求路由服务开启。 |
| `autoFailoverEnabled` | `false` | 对第三方 API 请求使用完整优先队列；关闭时只使用当前供应商。 |

- 首次开启自动故障转移要求服务与接管均已开启。保存设置、让开关从关变为开时立即启用队列 P1。
- 关闭接管会先恢复直连，监听服务可以继续运行。关闭路由总开关会恢复直连并关闭接管，
  但保留自动故障转移偏好、队列和参数。
- 官方登录使用独立的原生官方路由，自动队列暂不参与；这不要求删除原有第三方队列。
- 手动切换第三方供应商、编辑当前配置、切换官方账号时，完整原操作包在
  `with_provider_change` 中：暂时撤下本地配置覆盖，完成原操作，再按新的逻辑供应商恢复接管。
  不再沿用初版“手动切换就关闭全部自动切换”的行为。
- 运行状态的 `running`、`takeoverActive`、`autoFailoverActive` 不能互相代替。
  未接管、当前为官方账号或恢复失败时，保存的自动开关与实际自动路由状态可能不同。

CC Switch 的对应字段位于 `GlobalProxyConfig`、`AppProxyConfig`，由
[`commands/proxy.rs`](https://github.com/farion1231/cc-switch/blob/06082e189d65e6d6dbadc35dacdac1ce6c79d89a/src-tauri/src/commands/proxy.rs)
和 [`services/proxy.rs`](https://github.com/farion1231/cc-switch/blob/06082e189d65e6d6dbadc35dacdac1ce6c79d89a/src-tauri/src/services/proxy.rs)
实现。Codex-X 按 `CODEX_HOME` 隔离设置和运行实例；CC Switch 的设置按应用类型保存。
此外，CC Switch 在最后一个应用取消接管时会尝试停止服务，Codex-X 保留独立监听开关的含义。

## 完整队列与 P1

`providerIds` 保存 **P1、P2、P3……整个队列**，不是“当前主供应商之外的备用列表”。
当前供应商也可以加入；队列允许只有一家。新请求在自动模式下从 P1 开始，跳过暂时被熔断的项。
成功使用后面的项后，逻辑当前供应商会更新，但下一次请求仍按队列优先级选择。

开启自动模式时，空队列只会自动加入当前已保存、可用于路由的第三方供应商。
如果当前是官方账号或尚未保存的临时供应商，需要先选定有效 P1。P1 校验与启用失败不能留下
“界面已经开启、实际目标却未切换”的半完成状态；Codex-X 使用配置及状态检查点进行恢复。

队列可在停止状态编辑。已经开启自动模式后，移除最后一个成员不会偷偷重填当前供应商；
没有可用目标时请求返回 503。已删除或无效的供应商不再参与转发。供应商保存、删除、导入会刷新
运行配置，后台也定期检查外部变化。

没有“必须配置相同模型 ID”的入队限制。路由不修改请求中的 `model`，也不会把它替换为每家
供应商的默认模型；该模型是否可用仍取决于相应上游。参数不兼容等非重试错误会直接返回。

队列顺序在 Codex-X 的路由页独立保存，最多 64 项；这是资源上限。CC Switch 使用
`providers.in_failover_queue`，并按供应商列表的 `sort_index`、ID 排序，没有同模型或至少两家的限制。
对应源码为
[`commands/failover.rs`](https://github.com/farion1231/cc-switch/blob/06082e189d65e6d6dbadc35dacdac1ce6c79d89a/src-tauri/src/commands/failover.rs)
和 [`database/dao/failover.rs`](https://github.com/farion1231/cc-switch/blob/06082e189d65e6d6dbadc35dacdac1ce6c79d89a/src-tauri/src/database/dao/failover.rs)。

## 参数默认值与范围

配置版本为 2，IPC 使用 camelCase、平铺字段。下表是 Codex 的参数，不是 CC Switch 中 Claude
那组不同的默认值。前端与 Rust 服务端都会校验；界面错误率使用百分数，持久化使用 0–1。

| 字段 | 默认值 | 可配置范围 / 含义 |
| --- | --- | --- |
| `listenAddress` | `127.0.0.1` | IPv4、IPv6 字面量或 `localhost`；不接受任意主机名。 |
| `listenPort` | `15721` | 整数 1024–65535。 |
| `providerIds` | `[]` | 有序供应商 ID，最多 64 项，不接受重复、空值或官方账号 ID。 |
| `maxRetries` | `3` | 0–10；一次请求最多尝试 `maxRetries + 1` 个候选，每家至多一次。 |
| `streamingFirstByteTimeout` | `60` 秒 | 1–120 秒；分别用于等待响应头及首个响应数据块。 |
| `streamingIdleTimeout` | `120` 秒 | 0–600 秒；流式相邻数据读取间隔，0 表示不设置静默超时。 |
| `nonStreamingTimeout` | `600` 秒 | 60–1200 秒；单次非流式上游尝试的总期限，包括接收正文。 |
| `circuitFailureThreshold` | `4` | 1–20；达到连续失败次数即熔断。 |
| `circuitSuccessThreshold` | `2` | 1–10；半开探测达到此成功次数后关闭熔断。 |
| `circuitTimeoutSeconds` | `60` 秒 | 0–300 秒；从熔断到允许探测的等待时间。 |
| `circuitErrorRateThreshold` | `0.6` | 0–1，即界面的 0–100%；达到该错误率可触发熔断。 |
| `circuitMinRequests` | `10` | 5–100；错误率判断所需的最少样本数。 |

监听地址、端口只能在停止服务后修改。`localhost` 规范为 `127.0.0.1`；监听所有网卡时，
写入本机 Codex 的连接地址使用回环地址：`0.0.0.0 → 127.0.0.1`，`:: → ::1`。
IPv6 URL 使用方括号，实际绑定使用 `IpAddr`，不照搬上游裸 IPv6 字符串拼接的问题。

自动故障转移关闭或当前为官方账号时，运行路径只尝试当前目标，不使用上述熔断策略；
响应头等待和非流式请求采用 600 秒默认期限，不设置流式首数据块与静默期限。
本地入站读取、连接建立和向客户端写出仍有独立资源保护，不能将此理解为所有等待都无限制。

上游依据：
[`database/dao/proxy.rs`](https://github.com/farion1231/cc-switch/blob/06082e189d65e6d6dbadc35dacdac1ce6c79d89a/src-tauri/src/database/dao/proxy.rs)、
[`proxy/types.rs`](https://github.com/farion1231/cc-switch/blob/06082e189d65e6d6dbadc35dacdac1ce6c79d89a/src-tauri/src/proxy/types.rs)、
[`AutoFailoverConfigPanel.tsx`](https://github.com/farion1231/cc-switch/blob/06082e189d65e6d6dbadc35dacdac1ce6c79d89a/src/components/proxy/AutoFailoverConfigPanel.tsx)。

## 请求重试与熔断

一次请求持有开始时的路由、凭据和参数快照。改队列或参数影响后续请求，不把一条正在处理的请求
换成另一套凭据。被熔断或被其他半开探测占用的候选不消耗实际尝试次数。

连接失败、响应头等待失败、尚未输出时的首包失败及可重试 HTTP 错误可尝试下一家。
HTTP 400–599 中，以下状态不进入候补重试：`400/405/406/413/414/415/422/501`。
这些请求错误不累计供应商熔断失败。3xx 不跟随重定向，也不携带凭据跳转到另一地址。

拿到首个流式数据块后才提交下游响应；提交响应头或开始输出后，不再重试整条请求。
随后中断只结束当前流并更新错误提示，避免重复生成和重复工具执行。非流式正文在本次尝试的
总期限内完整读取；不是每收到一块数据就重新开始总计时。

带 `previous_response_id`、`conversation`、`background=true`、输入引用、文件 ID、加密内容或
不透明轮次状态的请求，只尝试**本次配置快照的队首**，不会失败后跨候补重放。
实现没有保存“历史 response ID / session → 原供应商”的长期绑定，不能承诺识别每段历史状态的
原始账户。此前一次请求在 P2 成功，并不意味着之后所有含历史状态的请求都自动绑定到 P2。

熔断器保留上游状态机：

| 状态 | 行为 |
| --- | --- |
| `closed` | 允许请求；连续失败达到阈值，或达到最少样本后错误率达到阈值，则转为 `open`。 |
| `open` | 等待恢复时间；到期后允许进入 `half_open`。 |
| `half_open` | 同时最多放行一个真实请求进行探测；失败立即重新熔断，成功达到恢复阈值后转为 `closed`。 |

探测由后续真实请求触发，不是定时向供应商发送额外测试消息。错误率使用当前熔断器生命周期的
累计样本，不是按分钟滚动的时间窗口；恢复关闭或手动重置会清空相应统计。
单项自动队列同样使用熔断器。关闭自动模式和官方路由绕过熔断器。

修改熔断参数会热更新已有实例，不借此清空失败记录。更换供应商的实际连接或凭据会重建对应健康
状态；移出运行路由、停止监听后，也不能把旧内存统计当作持续存在的探测结果。
「撤销修改」（恢复最近保存值）与「重置健康状态」是不同操作；后者不会主动验证服务已恢复。

上游依据：
[`provider_router.rs`](https://github.com/farion1231/cc-switch/blob/06082e189d65e6d6dbadc35dacdac1ce6c79d89a/src-tauri/src/proxy/provider_router.rs)、
[`forwarder.rs`](https://github.com/farion1231/cc-switch/blob/06082e189d65e6d6dbadc35dacdac1ce6c79d89a/src-tauri/src/proxy/forwarder.rs)、
[`circuit_breaker.rs`](https://github.com/farion1231/cc-switch/blob/06082e189d65e6d6dbadc35dacdac1ce6c79d89a/src-tauri/src/proxy/circuit_breaker.rs)。

## 原生官方账号隔离

官方登录可以接入本地 HTTP/SSE 路由，但不能加入第三方自动队列。切换到官方账号时，保留已保存
的自动偏好和队列，实际只运行当前官方账号；不会拿 OAuth 请求尝试第三方候补。

- Codex 继续拥有登录和 token 刷新。本模块不实现设备码登录，不主动刷新 token，也不写 `auth.json`。
- 本地官方配置使用 `requires_openai_auth=true`，关闭 WebSocket，并通过
  `x-codex-x-route-token` 验证本地调用者；不把 OAuth 替换为第三方 API Key 占位符。
- 每次请求检查逻辑官方状态、当前选中的 profile、实时认证文件中的精确 token 和 account ID。
  同一 Team workspace 下不同用户的 token 也不能混用；不从未选中账号快照借用认证。
- OAuth 只发往 `https://chatgpt.com/backend-api/codex`；官方 API Key 认证只发往
  `https://api.openai.com/v1`。拒绝混用两种认证，实际目的地址以请求时通过验证的认证类型为准。
- `x-codex-x-route-token` 和内部配置代次头不会转发上游，供应商自定义头也不能重新注入它们。
- 官方状态、邮箱/套餐摘要、额度查询、登录监控和快照捕获读取解包后的逻辑配置；不能把 localhost
  识别为新第三方供应商，也不能把本地路由令牌保存为账号凭据。
- 官方 `GET /v1/models` 只读取当前引用且经过归属检查的本地受管模型目录，否则返回 `{"models":[]}`。
  不请求外部模型目录，也不宣称它是完整的官方模型发现接口。尚未登录时可建立接管配置，实际请求仍须登录。

这对应 CC Switch 的 `apply_codex_official_proxy_route`、`is_codex_official_provider` 和官方认证透传分支，
不是 `codex_oauth_auth.rs` 中为其他客户端代管设备码登录／刷新、再转换请求的 OAuth 反代模式。
参考
[`codex_config.rs`](https://github.com/farion1231/cc-switch/blob/06082e189d65e6d6dbadc35dacdac1ce6c79d89a/src-tauri/src/codex_config.rs)
及 [`providers/codex.rs`](https://github.com/farion1231/cc-switch/blob/06082e189d65e6d6dbadc35dacdac1ce6c79d89a/src-tauri/src/proxy/providers/codex.rs)。

## 配置恢复、切换与退出

接管先保存恢复记录，再条件原子写入本地传输字段。恢复记录包含原始供应商表、监听位置、认证
令牌和配置代次标识，按 `CODEX_HOME` 隔离。恢复使用原始、已安装和当前值进行比较，保留随后
编辑的其他字段，并清除仍属于本功能的地址、令牌及代次头。历史恢复记录可识别旧备份中的本地路由。

成功转到队列其他供应商后，后台检查运行 revision、令牌、配置归属与队列成员身份，再更新逻辑
当前供应商和恢复记录。后台通知另外校验监听实例身份，停止后重新启动也不能将旧请求误认为新实例
的结果。过期请求不能覆盖之后的手动选择。不会因此重写当前请求的模型名、会话数据或 MCP、desktop 等通用配置。界面只静默读取当前目录状态，编辑中的表单暂缓刷新，避免覆盖草稿。

启动时先处理遗留的受管配置，再按保存的监听／接管偏好恢复服务。如果供应商已在外部改变，不
强行接管新配置；启动恢复失败时报告具体原因。退出先恢复直连，再停止监听；恢复失败
保留可服务的实例并阻止普通退出。成功退出后拒绝排队的重新启用请求。

关闭窗口继续驻留托盘。停止监听不主动截断已经开始输出的流，但停止后不再发起新的上游尝试；
退出整个进程仍不能保证未完成流继续。首次接管、关闭接管或更换账号后，已经运行的 Codex 可能缓存
原配置；需要重新打开客户端或新建会话的情况不能通过修改文件强制消除。

这些恢复保护是 Codex-X 保留的实现约束。参考版本的 CC Switch 在退出时先停服务再恢复，恢复失败
主要记录日志后继续退出；本项目没有照搬这种失败路径。

## 适用边界与实现差异

- 当前数据面提供 `POST /v1/responses`、`POST /v1/responses/compact`、`GET /v1/models`，使用 HTTP/SSE，
  不提供 WebSocket 升级或任意 URL 转发。
- 第三方接口需兼容 Responses。没有 Chat Completions／Anthropic 协议转换、请求整流器、图片降级、
  Claude/Gemini/Grok 应用接管或跨应用配置管理；不能把这些上游能力写成本项目已具备。
- 队列不限制模型名称，但不保证任意第三方都支持请求中的模型、工具或服务端状态。
  厂商专用 `query_params` 目前不支持自动路由；无效认证、缺失环境变量或不适用的认证头会报告原因。
- 本地监听即使绑定非回环地址，也保留随机令牌、Host 和来源检查。不是无需认证的公开代理。
  不跟随重定向，不向第三方传递官方账号头、Cookie、本地令牌或其他账户凭据。
- 入站头最多 32 KiB，正文及每层解压结果最多 64 MiB，查询串最多 4 KiB；使用 8 个有界工作线程。
  支持 gzip、deflate、zstd 请求解码，不记录原始请求正文或 token。
- 运行统计是传输层统计：完整非流式响应或首个流式输出计为一次成功；之后的流中断另行报告。
  这不是对完整答案质量或整个工具调用流程成功的判断。该统计也不是订阅额度或金额统计。
- 本轮未提供 CC Switch 的请求日志、成本计费及其他应用的开关。已有「用量统计」与官方额度查询
  仍由各自功能负责。

## 代码与许可来源

本项目对应实现为 `failover/config.rs`、`controller.rs`、`proxy.rs`、`circuit_breaker.rs`、
`native_official.rs`，以及命令、官方配置读取和前端事件接线。

熔断器状态机与部分压缩处理按 CC Switch 上述提交的 MIT 代码适配；保留其版权声明
`Copyright (c) 2025 Jason Young`。同步锁、有界本地 HTTP 处理、状态持久化、配置恢复及界面接入
按 Codex-X 的现有结构实现。完整许可和来源见 [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)。

验证使用临时目录、合成认证和本机 HTTP/SSE 服务，覆盖参数校验、P1 与队列、熔断、超时、
已提交输出后的禁止重放、并发切换、恢复失败、原生官方隔离、模型目录归属和草稿保留。
本轮 598 项 Rust 测试、51 项前端测试、类型/格式/diff 检查及 macOS `.app` 构建通过；
浏览器检查已覆盖三层开关、队列排序、不同模型、参数编辑、熔断重置和明暗主题。
本地 macOS App 已校验替换并启动，实际安装版验证了页面读取及监听服务独立启停，
结束后恢复原关闭状态，现有 config.toml 与 auth.json 哈希未变；尚未验证 Windows 原生安装包。
