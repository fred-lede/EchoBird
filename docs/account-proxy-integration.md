# 账号反代与 Kilo / Copilot 接入

账号反代将 EchoBird 保存的账号作为智能路由的模型来源。账号凭据只由后端读取，模型配置保存账号来源和模型 ID，API 密钥为空。

## 添加账号模型

1. 在工具页或账号中心添加账号。支持 Claude Code、ChatGPT / Codex、WorkBuddy、WorkBuddy AI、Antigravity、Kilo、GitHub Copilot。
2. 打开智能路由中间的「反代号池」页签，点击确认句中带下划线的「风险须知」查看说明，并自行勾选「我已阅读风险须知，并自愿承担风险」右侧的勾选框。未勾选时，产品列表置灰，列表和右上角添加按钮均不可用；确认状态在本机保存，也可取消勾选。查看或关闭须知不会自动确认。确认后点击对应产品。产品右侧显示 EchoBird 保存的账号数量；读取失败时保留已有数量，尚未读取成功时显示「—」。数量读取不查询额度或访问原生客户端。
3. 选择保存的账号，首次自动读取模型列表；读取成功后按工具和账号保存列表，再次添加或编辑时复用。使用下拉菜单选择模型，模型栏的刷新按钮可重新读取列表，刷新失败保留已有列表和选择。账号反代的添加和编辑表单不显示 API 密钥行，也不接受手动输入模型 ID。
4. 在工具页选择智能路由并应用。现有 API 模型继续使用原来的配置流程。

打开页签只读取保存的账号；添加或编辑表单在已选择账号且没有缓存时自动读取模型目录，有缓存时不发送目录请求。刷新额度、登录和应用仍由用户明确触发，表单不会启动登录、查询额度或修改客户端。删除绑定的账号后，该模型请求会报告账号失效；重新编辑并绑定有效账号即可恢复。

Kilo CLI 与 Desktop 共用官方认证存储，但配置与模型选择独立。Desktop 使用自己的默认模型偏好，已有会话的模型不随默认设置重写。Copilot CLI 与 Desktop 的活动账号分别标记，避免把应用到其中一端误显示为另一端已切换。

## 必要的协议差异

| 来源                     | 请求实现与限制                                                                                                                                                                                                                            |
| ------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Claude Code              | 使用官方 CLI 的隔离运行目录和 OAuth 环境，通过私有 MCP 桥接调用方提供的工具；工具结果及其后的用户消息继续同一运行会话，需要安装可用 CLI。                                                                                                 |
| ChatGPT / Codex          | 使用账号的 Responses 接口，保留工具调用、工具结果和不透明 reasoning 状态。智能路由关闭 Codex 自带联网搜索；普通 API 模型设置保留。                                                                                                        |
| WorkBuddy / WorkBuddy AI | 使用各自账号和目录。指定单个函数时转换为 `required` 并收窄工具列表，匹配上游实际协议。                                                                                                                                                    |
| Antigravity              | 使用账号目录和 Google 请求协议；`free-tier` / `legacy-tier` 不作为 `entitlement.userTier` 提交。                                                                                                                                          |
| Kilo                     | 使用账号的模型目录和兼容接口；CLI / Desktop 账号认证来源一致。                                                                                                                                                                            |
| GitHub Copilot           | 使用官方 CLI 登录保存的账号，依据账号目录中的 `supported_endpoints` 选择 Chat、Responses 或 Anthropic Messages 接口；保留目录提供的 `/v1/messages` 或 `/messages` 路径。Desktop API 配置通过官方应用的本地认证 WebSocket 写入原生凭据库。 |

Copilot 的 Anthropic 接口将 `required` 转为 `any`，指定函数转为 `tool` 并保留函数名，`auto` / `none` 保持对应语义；禁止并行调用时使用 `disable_parallel_tool_use`。上游拒绝某模型的强制工具选择时，保留原错误，不降级为自动选择。映射依据 [官方工具选择合同](https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools) 与 [并行工具调用合同](https://platform.claude.com/docs/en/agents-and-tools/tool-use/parallel-tool-use)。

Copilot 目录中的 `model_picker_enabled` 表示官方模型选择器的显示状态，不作为 API 访问权限判断；仍排除明确禁用、隐藏或不支持 API 的模型。旧模型未提供 `supported_endpoints` 且 `capabilities.type` 为 `chat` 时，兼容使用 Chat Completions；明确提供空端点列表或非聊天类型时不套用此回退。账号 API 端点使用显式刷新保存的 `endpoints.api`，不会在被动导航时解析端点或刷新额度。

Claude 桥接同样按每次请求映射工具策略，指定函数使用私有 MCP 的对应名称。续轮先将追加消息作为独立用户输入排队，等待本次策略更新成功，再交回工具结果；不会把用户消息拼入工具返回内容。CLI 的初始化目录表示客户端提供的模型能力，不代表该订阅账号的使用权限已验证；真实权限仍需账号请求验收。实现依据官方 CLI 2.1.92 公开源码及 Agent SDK 0.3.296 的设置控制合同，当前安装的原生 CLI 2.1.296 仍待实机核验。[官方流式输入说明](https://code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode)、[官方 SDK](https://github.com/anthropics/claude-agent-sdk-typescript)

Copilot Desktop 在关闭时，明确应用配置会临时启动官方应用，等待认证桥就绪，写入配置并关闭此次启动的进程。已运行的应用保留运行。被动账号加载和配置读取不会启动应用。应用按钮在配置写入及可选启动完成前保持等待，避免冷启动期间重复应用。

## 平台与验证边界

下表中的「实现」表示对应路径、配置和协议已接入；Windows 的构建与 fixture 不代表 macOS / Linux 原生客户端验证通过。

| 能力                                   | Windows                                                                                                                                         | macOS                                      | Linux                                |
| -------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------ | ------------------------------------ |
| Kilo CLI 检测 / 安装入口 / 启动        | 实现；检测到 Desktop 内置 7.7.9，原生 CLI 配置解析通过；独立安装和交互终端待实测                                                                | 实现；原生验证待进行                       | 实现；原生验证待进行                 |
| Kilo Desktop 检测 / 安装入口 / 启动    | 实现；检测到 0.1.11；原生 GUI 可见启动及此次启动的进程清理实测通过，原生配置与无关进程保持不变                                                  | Apple Silicon 实现；Intel 厂商未提供客户端 | 实现；原生验证待进行                 |
| Kilo 两端登录 / 账号切换 / API 选模    | 实现及 fixture；原生 API 选模及同账号应用实测通过、配置逐字节恢复；用户确认浏览器新登录成功，CLI / Desktop 共用已保存账号来源；多身份切换待实测 | 实现及通用 fixture；原生验证待进行         | 实现及通用 fixture；原生验证待进行   |
| Copilot CLI 检测 / 安装入口 / 启动     | 实现及 fixture；本机未安装独立 CLI，官方 Desktop SDK 运行时的实际连接与 ping 实测通过；官方浏览器回调及 EchoBird 账号保存实测通过               | 实现；原生验证待进行                       | 实现；原生验证待进行                 |
| Copilot Desktop 检测 / 安装入口 / 启动 | 实现；检测到官方签名 1.1.28，实际启动及本地配置桥通过                                                                                           | 实现；原生验证待进行                       | x64 / ARM64 实现；原生验证待进行     |
| Copilot 两端登录 / 账号切换 / API 选模 | 实现及 fixture；Desktop 关闭时重复 API 应用 / 读取 / 恢复 / 进程树清理实测通过；官方浏览器回调及 EchoBird 账号保存实测通过；多身份切换待实测    | 实现及通用 fixture；原生验证待进行         | 实现及通用 fixture；原生验证待进行   |
| 账户目录与代理                         | 实现及 fixture；下表列出实际请求结果                                                                                                            | 通用 HTTP / CLI 实现；原生验证待进行       | 通用 HTTP / CLI 实现；原生验证待进行 |

Kilo 平台发行范围以 [官方安装页面](https://kilo.ai/install) 为依据。检查时独立 CLI 已发布 [7.8.8](https://github.com/Kilo-Org/kilocode/releases/tag/v7.8.8)，本机 Desktop 内置版本为 7.7.9；两者更新周期不同，本次没有升级用户安装的客户端。Copilot Desktop Linux x64 / ARM64 安装入口依据 [官方 1.1.28 发行文件](https://github.com/github/app/releases/expanded_assets/v1.1.28)。

## 请求验证

Windows 使用已保存的有效账号，通过实际智能路由逐项检查文本、流式响应、强制函数调用和工具结果续轮。探针使用内存中的路由候选及健康状态，不修改用户的路由配置。真实浏览器授权需要账号持有人操作，捕获已有原生认证不代替新登录验证。

| 来源            | 目录                                                                        | 文本 / 流 / 工具 / 工具结果续写（`none`）                             |
| --------------- | --------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| ChatGPT / Codex | 7 个模型                                                                    | `gpt-6-luna` 四项通过                                                 |
| WorkBuddy       | 18 个模型                                                                   | `glm-5.3-flash` 四项通过                                              |
| WorkBuddy AI    | 26 个模型                                                                   | 国际版实际生产路由四项通过                                            |
| Antigravity     | 26 个模型                                                                   | `gemini-2.5-flash` 四项通过                                           |
| Kilo            | 390 个模型                                                                  | `kilo-auto/free` 四项通过                                             |
| Claude Code     | 已安装官方 CLI 控制初始化通过，使用无效 fixture token，仅证明能力目录可读   | 用户无付费账号，本次不实测；真实账号权限、订阅请求及 MCP 续跑仍未验证 |
| GitHub Copilot  | 官方浏览器回调及账号保存通过；29 个模型，含 13 个未提供端点列表的旧聊天模型 | `gpt-4.1` 四项通过                                                    |

2026-10-12 合并后的实际生产路由四项检查通过：Codex 7.61 秒、WorkBuddy 国内版 15.23 秒、WorkBuddy AI 国际版 16.96 秒、Antigravity 8.71 秒、Kilo 9.76 秒、Copilot 16.19 秒；目录数量与上表一致。用户已确认 WorkBuddy AI 登录成功，并明确确认 Windows Kilo 新登录成功。Kilo CLI / Desktop 共用 EchoBird 保存的账号来源；多身份切换仍未实测。

Copilot 登录后暴露的配置兼容问题已修复：支持原生配置中的 JSONC header，并兼容新版 `authTokens[host:login] = { token: string }` 与旧版 `copilotTokens` 字符串映射，仅按精确身份键读取。官方浏览器回调已返回，EchoBird 已成功保存真实账号行。目录原始返回 52 个模型，修正 picker 标志判断后筛选得到 16 个；补充 13 个旧 Chat 模型后，真实目录读取于 1.34 秒通过，共 29 个模型。账号实际 API 端点上的 `gpt-4.1` 文本正控于 5.24 秒通过；随后通过生产智能路由完成四阶段，耗时 16.19 秒，未覆写协议，并验证路由标记及独立随机工具结果。其目录行确实未提供 `supported_endpoints`，且未被 policy、API 或隐藏状态禁用。此前 `gpt-5-mini`、`gpt-6-luna`、`claude-haiku-4.5` 的尝试返回上游 HTTP 400 `model_not_supported`，这些模型不能记为推理通过，也不能据此推断账号套餐。

已确认 Copilot CLI OAuth 凭据直接用于账号 API 请求，编辑器会话令牌交换属于不同认证流程；账号 API 端点由显式刷新保存，未提供端点列表且声明聊天能力的旧模型回退到 Chat Completions。上述兼容行为由本地 fixture 和本节列出的 Windows 请求结果分别验证，不代表其他模型、订阅权限或 macOS / Linux 原生客户端已验证。

Kilo 首次工具调用返回的自然语言 `marker` 参数不符合探针预期；将工具 schema 的参数约束为 `enum: ['probe-ok']` 后完整通过。这是测试约束改进，没有修改生产适配。Copilot 本地 HTTP fixture 覆盖三种协议的目录签名、端点选择、流式函数调用和工具结果续轮，并检查不会查询额度；fixture 不代表真实账号认证或订阅请求通过。

复现请求探针需显式指定 `ECHOBIRD_LIVE_PROXY_PROVIDER`、`ECHOBIRD_LIVE_PROXY_MODEL` 和 `ECHOBIRD_LIVE_PROXY_INFERENCE=1`，只运行 `live_saved_account_inference_probe` 这个 ignored 测试。目录探针为 `live_saved_account_catalog_probe`。这些探针不打印凭据，也不会自动登录或切换原生账号。最新探针要求续轮返回仅存在于工具结果中的随机值，并检查 `none` 不产生新的工具调用；上表六类通过记录均使用这些加强后的断言。

已将其他开发窗口的 14 个提交合并到 `codex/account-proxy-acceptance-20261012`，当前 HEAD 为 `e1b7af02`；本轮账户代理接入改动尚未提交。Copilot 配置与目录兼容修复后，完整重跑前端类型、格式、Lint 和测试通过：59 个测试文件、860 项通过。Rust 格式检查、全 target / feature Clippy（`-D warnings`）和完整测试通过：共 813 项，787 项通过、0 项失败、26 项显式实机测试默认跳过。

Claude 新增 9 项隔离回归测试，覆盖同一运行会话的工具续轮、独立用户输入、设置 ACK 屏障、工具选择重置与限制、缺失或重复结果、取消及失败清理；fixture 即使错误走到新会话分支，也会禁止启动已安装的 CLI。Windows 心跳测试等待实际数值递增；路由停止测试在旧请求未完成时立即重绑端口并验证新服务响应，之后确认旧请求正常完成。两项测试的时序检查已修正，生产进程停止逻辑保持原有实现。

Kilo 原生 GUI 探针 11.56 秒通过，包括 Form 检测正控、可见的原生窗口及此次启动进程的清理；无关进程和原生配置哈希保持不变。Copilot 实际生产运行时探针 1.45 秒通过，验证协议 3 连接、带原 nonce 的 `pong` 和有效时间戳；6 个原生路径哈希保持不变，内置 SDK 进程无残留。这些探针没有验证新 OAuth 登录或真实订阅推理。Kilo 与 Copilot 的原生配置探针另行显式运行，结束后恢复原始配置。

用户已通知另一开发窗口结束并授权实机验收。当前运行合并源码的 Tauri 开发客户端，本轮记录的 PID 为 `9804`，可执行文件为 `D:/EchoBird-build/4b2f/target/debug/echobird.exe`，前端地址为 `http://127.0.0.1:5176`。开发客户端中，Copilot 选择已保存账号后明确读取模型成功，列表包含全部 29 项及 `gpt-4.1`，API 密钥保持禁用。检查结束取消表单，没有保存新模型；现有 11 个路由候选（包含用户新增的 WorkBuddy）保留，路由未启用。按用户要求直接启动开发客户端，本轮没有生成新的安装包，也未提交或推送。此前 2026-10-11 的 NSIS 安装包及静态产物检查属于整合前版本，不能作为合并后安装包的验证证据。

## 维护入口

智能路由的原生 Responses 优先转发、兼容转换及续接限制见 [Responses 请求说明](./responses-routing.md)。此前请求验收记录仍表示各账号在当时的生产 Chat 路径通过，不代表本轮所有原生 Responses 功能已完成真实订阅验收。

- 账号目录与凭据隔离：`src-tauri/src/services/account_model_sources.rs`。
- 请求协议与 CLI 桥接：`src-tauri/src/services/account_proxy/`。
- 智能路由重试、来源冷却和 Responses 入口：`src-tauri/src/services/smart_router/server.rs`。
- 共享模型表单：`src/pages/ModelNexus/AccountModelFields.tsx`；入口：`src/pages/FreeModels/AccountProxyPanel.tsx`。
- Kilo / Copilot 账号生命周期：`src/pages/AppManager/useClientAccounts.ts`，复用 `useManagedAccounts` 与 `AccountSectionPrimitives`。
- 新工具的 bundled catalog 在 `tools/`，远程安装 catalog 在 `docs/api/tools/install/`，两端必须同步。
