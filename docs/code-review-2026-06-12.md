# cc-switch 全量代码审阅报告（2026-06-12）

> 审阅方式：5 个并行深读代理分别覆盖「代理转发核心 / 协议转换层 / services 业务层 / 命令·配置·数据库层 / React 前端」，外加横切面全局扫描。代码规模：Rust ≈ 118,900 行（192 文件）、TS/TSX ≈ 63,000 行（288 文件）。
> 汇总版优化建议见本文件末尾「优先级路线图」，或对话记录。

---

## 〇、横切面全局扫描（主控发现）

- 前端产物为**单个 3.6MB JS chunk**（dist/assets/index-*.js），无任何代码分割：全项目 0 处 `React.lazy`，vite.config.ts 无 `manualChunks`；recharts（仅用量图表用）、CodeMirror（仅编辑器用）、framer-motion（11 个文件）全部打进首屏。
- 568KB i18n 语言包（en/ja/zh/zh-TW 各 130-171KB）全量同步打包，未按语言懒加载。
- 巨型图标资源直接入包：src/icons/extracted/relaxcode.png **1.1MB**、sudocode.png 436KB、shengsuanyun.svg 216KB——供应商图标应压缩到 10-30KB（webp/缩尺寸）。
- Rust 非测试代码约 **1000 处 `.unwrap()` + 1000 处 `.expect()`**（含部分测试模块，量级如此）。
- 文件 IO：`std::fs` 301 处，`tokio::fs` 0 处，`spawn_blocking` 仅 24 处——大量同步 IO 跑在 async 上下文。
- tsconfig 已开 strict（好）；TS `any` 仅 70 处，集中在少数文件。

---

## 一、代理转发核心（src-tauri/src/proxy/ 顶层）

### 【高】影响

**1. SQLite 单连接 + `std::sync::Mutex` + 未开 WAL，阻塞 I/O 直接跑在 async 热路径上**
- 位置：`src-tauri/src/database/mod.rs:76-78`；`src-tauri/src/proxy/provider_router.rs:83-106,235`；`src-tauri/src/proxy/handler_context.rs:95-103`
- 问题：整个应用共享一个 `Mutex<rusqlite::Connection>`，无 `journal_mode=WAL` / `busy_timeout` 设置（默认 rollback journal，每次写整库锁 + fsync）。每个请求在 `RequestContext::new` 里同步读 proxy_config / rectifier_config / optimizer_config，`select_providers_for_session` 再同步读 providers / failover_queue / keys / affinity，全部直接在 tokio worker 线程上做阻塞磁盘 I/O。
- 影响：并发场景下所有请求在这把锁上串行；写操作与读路径互相阻塞，阻塞调用卡住 runtime worker，放大尾延迟。
- 建议：启动时执行 `PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; busy_timeout`；热路径 DB 调用包 `spawn_blocking` 或引入专用写线程+队列；per-app 配置加内存缓存（更新命令触发失效），它们当前每个请求被读 2-3 次。

**2. 成功路径在把响应交还给客户端之前同步做 2-3 次 DB 写**
- 位置：`src-tauri/src/proxy/forwarder.rs:265-341`（`record_success_result`），调用点 `677-683`
- 问题：每次成功请求，`bind_working_channel_affinity`、`bind_session_affinity`、`record_key_success` 都同步完成后才返回；只有 `record_channel_result` 走了 `tokio::spawn`。这 2-3 次写直接叠加在流式响应的首包延迟（TTFB）上。
- 建议：照 `record_channel_result` 的样子把 affinity 绑定与 key 成功记录整体 `tokio::spawn` 出去；只有 HalfOpen permit 释放需要保持同步。

**3. 原生 Claude（anthropic 格式）路径每个请求都新建 TCP+TLS，无连接复用**
- 位置：`src-tauri/src/proxy/hyper_client.rs:358-424,651-687`；`src-tauri/src/proxy/forwarder.rs:1851-1912,2290-2300`
- 问题：`should_preserve_exact_header_case` 对 anthropic 格式恒为 true → 走 `send_raw_request`：每请求 DNS + TCP + rustls 握手，响应读完连接即弃。这是使用频率最高的路径，每请求多 1-2+ RTT 和一次 TLS 握手 CPU。
- 建议：按 host 维护小型空闲 `SendRequest` 池；或把"字节级 header 保真"降级为 provider 级配置开关，默认走 reqwest 池。

**4. failover 状态机漏洞：session affinity 置顶 + key 池场景会永久跳过中间 provider**
- 位置：`src-tauri/src/proxy/forwarder.rs:578-586`（pending 跳过）、`1284-1287`/`1343-1346`（设置 pending）；`src-tauri/src/proxy/provider_router.rs:223-227`（affinity `insert(0)` 置顶）
- 问题：attempts 顺序可能是 `[A:k2(affinity 置顶), B, A:k1]`。A:k2 key 级失败且 A 还有剩余 key 时设置 pending，循环对 B 直接 `continue`；走到 A:k1 时 pending 清除，但迭代器已越过 B 永不回头。若 A 的所有 key 都失败，配置在队列里的 B 根本没被尝试。
- 建议：去掉 pending 跳过机制，改为把 affinity 命中的 key 连同其同 provider 兄弟 key 整组前移；或将被跳过的 attempt 收集后在循环尾部补跑。

**5. keep-alive 连接上第 2+ 个请求复用首个请求的 header 大小写/顺序快照**
- 位置：`src-tauri/src/proxy/server.rs:156-192`；消费方 `src-tauri/src/proxy/hyper_client.rs:597-630`
- 问题：`stream.peek(8192)` 在 accept 时执行一次，`OriginalHeaderCases` 供该连接所有请求使用。客户端复用 keep-alive 时后续请求 header 集合不同则顺序错乱；首请求 header 超 8KB 也会截断。header 保真从连接第二个请求起静默失效，与 finding 3 的投入产出严重不对称。
- 建议：在每个请求边界重新捕获；或接受"仅对每连接首请求保真"并写明，同时重新评估 finding 3 的取舍。

### 【中】影响

**6. 请求体每次尝试约 3 次全树深拷贝 + 2 次全树重建**
- 位置：`src-tauri/src/proxy/handlers.rs:194`；`src-tauri/src/proxy/forwarder.rs:630-643`、`1450-1456`、`2695-2697`
- 建议：`forward()` 改为按值接收 `provider_body`；filter 与 canonicalize 合并为单次遍历；透传路径跳过 canonicalize。

**7. 整流重试的成功/失败记账逻辑 4 处复制粘贴，~300 行可删**
- 位置：`forwarder.rs:212-263`（已有 helper 仅 media/anyrouter 使用）vs `968-1027`、`1147-1199`（逐字重复）；终态失败块在 `908-931`、`1069-1094`、`1097-1122`、`1231-1253`、`1348-1373` 重复 5 次（success_rate 公式重复 8 次）。
- 建议：signature/budget 成功路径改调 `finalize_same_provider_retry_success`；抽 `fail_terminal` 统一失败收尾。

**8. `ProxyStatus` 单把 RwLock 是每请求 ~6-10 次写的热点；`ActiveConnectionGuard` Drop 靠 spawn 减计数**
- 位置：`forwarder.rs:58-84`、`502-506`、`654-658`、`695-721`
- 建议：计数字段拆成 Atomic（Drop 变无锁 `fetch_sub`），RwLock 只留字符串展示字段。

**9. hyper raw 路径的流式请求等响应头用的是 non_streaming 超时（默认 600s）**
- 位置：`forwarder.rs:1836-1840,1877-1893`（reqwest 用 streaming_first_byte_timeout）vs `1896-1912`（hyper 用 600s）
- 建议：流式请求把 `streaming_first_byte_timeout` 传入 hyper `send_request`，与 reqwest 分支对齐。

**10. 熔断器"错误率"是自上次 Closed 以来的终身累计，不是滑动窗口**
- 位置：`circuit_breaker.rs:266-284,380-387`
- 问题：长期运行后错误率触发条件形同虚设（靠连续失败阈值兜底）；u32 理论回绕。
- 建议：时间桶滑动窗口或定期衰减；或删掉错误率路径只留连续失败阈值。

**11. `is_key_scoped_error` 关键字匹配过宽，普通 4xx/5xx 会被误判为 key 维度问题**
- 位置：`forwarder.rs:2543-2568`
- 问题：任意状态码 body 含 "api key"/"quota"/"balance"/"credit"/"insufficient"/"rate limit" 即算 key-scoped，如 400 "max_tokens exceeds your credit plan limit" 会被转入"冷却 key + 换 key 重试"，打满整个 key 池。
- 建议：body 关键词判定限定在 401/402/403/429；关键词改精确短语。

**12. Codex/Gemini 四个 handler 的转发失败路径丢失 `key_id`**
- 位置：`handlers.rs:537-544,602-609,679-686,1211-1218`，对照 Claude 路径 `202-207`
- 建议：补 `ctx.provider_key_id = err.key_id.take()`；更好是把五处重复样板抽公共函数。

**13. SSE 检查缓冲区 O(n²) 扫描且无大小上限**
- 位置：`response_processor.rs:694-808`；`sse.rs:8-23`
- 问题：每 chunk 从 buffer 开头 `find("\n\n")`，大事件 O(n²)；buffer 无上限。
- 建议：记录上次扫描偏移；buffer 设上限（如 4MB），超限丢弃并对本流禁用 usage 收集。

### 【低】影响

**14. 每条 usage 日志做 4 次串行 DB 查询，无缓存**（`response_processor.rs:629-684`；`usage/logger.rs:206-288`）— 建议 per-app 默认值与 provider meta 加 TTL 缓存，model_pricing 全量载内存。

**15. `get_or_create_circuit_breaker` 持写锁跨 DB await**（`provider_router.rs:534-571`）— 先读配置再拿写锁。

**16. thinking 签名整流器"场景 7"触发过宽**（`thinking_rectifier.rs:100-108`）— 任何 "invalid request" 都触发剥 thinking 重试；建议加 thinking 相关词条件。

**17. 全部通道被熔断拒绝时错误类型退化**（`forwarder.rs:1379-1395`）— `attempted_channels==0` 返回泛化 `NoAvailableProvider`；应返回 `AllProvidersCircuitOpen` 并带恢复时间。

**18. forwarder.rs 应拆分；handlers.rs 五处端点样板重复**
- `forward_with_retry_inner` ~890 行单函数；`forward()` ~510 行混合多职责；anyrouter 特判散布通用路径。
- 建议：拆 `forwarder/{retry_loop, upstream_request, rectify_retries, channel_policy, anyrouter}`；handlers 抽泛型入口。

### 整体评价
领域设计成熟：通道级熔断与 key 级冷却分轨、Retry-After 感知、HalfOpen permit RAII、SSE 跨 chunk UTF-8 安全、usage 收集 Drop 兜底，测试覆盖上乘。但路由/记账层全压在无 WAL 的单连接 SQLite 互斥锁上、最热的 Claude 路径每请求重建 TLS，精心设计的故障转移跑在会自我排队的地基上。修复优先级：finding 4（正确性）→ 1/2（DB 热路径）→ 7（去重）→ 3/5（连接与 header 保真取舍）。

---

## 二、协议转换层（src-tauri/src/proxy/providers/）

（完整 18 条见下，原始报告由审阅代理落盘）

**发现 1【中】transform.rs:355-366** — image 块只支持 base64 source，url source 生成坏 data URI（`data:image/png;base64,` 空体）。按 `source.type` 分流。

**发现 2【中】transform.rs:399-455** — thinking 往返不对称：preserve 模式下 `reasoning_content` 只在 assistant 带 tool_calls 时注入，纯文本 assistant 历史思考静默丢失，A→B→A 不还原。

**发现 3【低】transform.rs:463-482** — clean_schema 逐属性 clone + 递归不覆盖 `anyOf/oneOf/allOf/$defs`；与 gemini_schema.rs 重复。改 in-place 递归并补全分支。

**发现 4【低】transform.rs:200-213** — BatchTool 过滤检查错字段（查 `type` 应查 `name`，大概率永不生效）；description 缺失序列化为 null（严格后端 400）；legacy `function_call` 路径生成空 tool_use id。

**发现 5【中】transform_codex_chat.rs:1040-1061** — `collect_tool_search_output_tools` 对整个 input 全量递归遍历（含历史大体积输出、base64），而 `tool_search_output` 只出现在顶层 item。只遍历顶层。

**发现 6【中】transform_codex_chat.rs:627-659** — 每请求对全部历史 tool 输出重新 parse+serialize（O(总字节) CPU 随历史线性涨）；`custom_tool_call_output` 把整个 item 含元数据序列化进 content，与 `function_call_output` 不一致，不可还原。

**发现 7【低】transform_codex_chat.rs:506-532** — `collapse_system_messages_to_head` 漏数组 content 的 system；与 transform.rs `normalize_openai_system_messages` 是两套漂移实现。抽公共 `merge_system_to_head`。

**发现 8【低】transform_codex_chat.rs:284-297** — `max_tokens`/`max_completion_tokens`/`max_output_tokens` 三个 if 独立，可能同时输出互斥字段（OpenAI 400）；reasoning effort 档位表与 transform.rs 重叠但不同。

**发现 9【中】transform_gemini.rs:278-331** — system 文本未剥离 x-anthropic-billing-header（transform.rs 为 #2350 专门写了 strip 函数），Gemini systemInstruction 每请求变化导致隐式缓存命中率归零，成本上升。复用同一 strip。

**发现 10【中】transform_gemini.rs:1096-1127** — usage 映射把缓存 token 双计：Gemini `promptTokenCount` 含 `cachedContentTokenCount`，而 Anthropic `input_tokens` 与 `cache_read` 互斥。应 `input = prompt - cached`（saturating_sub）。

**发现 11【低】transform_gemini.rs:162-269** — parts 三连 clone（含 base64 时三份大拷贝）；thought parts 全链路丢弃；`map_tool_choice` 对 "any" 直接 Err 500（transform.rs 映射为 "required"，跨后端行为发散）。

**发现 12【中】transform_responses.rs:300-433** — 多轮工具调用不回放 reasoning item：OpenAI 严格实现要求 function_call 前有配套 reasoning item，否则 400 或推理上下文丢失降智。参照 gemini shadow store 按 call_id 缓存回放。

**发现 13【低】transform_responses.rs:14-41,342-354,358-365** — "Read 工具空 pages" 修复硬编码进转换层（与 gemini rectify 是两套机制）；image url source 同发现 1；flush 处可用 `std::mem::take`；第 4 份 usage 映射。

**发现 14【高】streaming_responses.rs:162-771** — `response.failed`/`error` 事件落入 `_ => {}` 被静默丢弃，无任何终止事件：上游 mid-stream 报错时客户端只能等连接关闭报"流被截断"，真实错误信息全丢。streaming.rs:199 解析失败的 error 块同样静默丢；streaming_gemini.rs:405 传输层错误直接断流。四套里只有 streaming_codex_chat 正确处理。抽公共 `detect_upstream_error` 统一发 error 事件。

**发现 15【中】streaming_responses.rs:607-688** — 只认非标 `response.reasoning.delta`，官方 `response.reasoning_summary_text.delta`/`response.reasoning_text.delta` 全部丢弃；cc-switch 自产流（streaming_codex_chat 发的恰是官方事件名）喂给自己的转换器 thinking 全军覆没。

**发现 16【中】streaming_gemini.rs:356-399** — cumulative/incremental 二义启发式 `starts_with` 可静默吞字符（首 chunk "#"、下一 delta "#include..." 丢一个 "#"）；纯累积流每 chunk 重建全量字符串 O(n²)。

**发现 17【中】streaming_codex_chat.rs:196-221** — inline `<think>` 模式整段思考缓冲到闭合标签才发出：DeepSeek-R1 风格上游几十 KB 思考期间客户端零输出、buffer 无上限。可安全增量发出非尾部内容。

**发现 18【低】streaming.rs:142-653** — 515 行单 async 块、6 层嵌套、~20 处 `format!("event: ...")` 样板；message_start 兜底/索引分配/收尾清扫在三个出向转换器各写一遍。提取 `AnthropicSseEmitter`。

### 四套转换器共享抽象方案
1. **usage 归一化**：4 份实现收敛为 `struct NormalizedUsage { input, output, cache_read, cache_creation }` + 各协议 `from_xxx()/to_xxx()` 薄壳（顺带修发现 10）。
2. **system/instructions 处理**：strip billing header + 合并 system 统一成 `collect_system_text(value, sep)`（修发现 7、9）。
3. **工具定义与 tool_choice**：`enum ToolChoice` 为中枢；clean_schema 与 gemini_schema 合并为带 target 参数的单一 sanitizer（修发现 3、11c、4）。
4. **SSE 发射层**：全局唯一 `sse_event()` + `AnthropicSseEmitter` 状态机（修发现 14、18）。
5. **上游错误检测**：`detect_upstream_error` 共享。
6. **工具参数纠偏**：按工具名注册的规则表。

### 总评
转换层工程质量高于同类开源代理（UTF-8 安全缓冲、tool_call 索引路由、Gemini shadow store、Codex 工具名哈希截断、带 issue 号的回归测试）。主要债务是横向重复——usage/system/tool_choice/SSE 样板各 3-4 份且已出现行为漂移（发现 7、9、15 都是真 bug）；以及错误事件在三条流式链路被静默吞掉（发现 14）。优先修 14、9、10、15。

---

## 三、services 业务层（src-tauri/src/services/）

**【高】1. 同步路径 `block_on` 阻塞/死锁风险** — provider/mod.rs:1401,1417,1428,1969 与 live.rs:940,955、proxy.rs:245。同步函数里 `futures::executor::block_on` 等待 tokio Mutex 和 async DB/代理调用，从 runtime 线程进入会卡死 worker 甚至死锁。命令层改 async 或统一经 `tauri::async_runtime::block_on`。

**【高】2. 去重过滤器是相关子查询，O(n²) 全表扫** — usage_stats.rs:225-258 `effective_usage_log_filter` 对 proxy_request_logs 每行执行 EXISTS 指纹匹配，被 summary/trends/provider/model/logs 全部查询内嵌且持全局连接锁。写入时物化 dedup 标志列，或建复合索引。

**【高】3. 分页读接口隐式写库** — usage_stats.rs:1302-1305、1338-1341。`get_request_logs`/`get_request_detail` 读路径对每行调 `maybe_backfill_log_costs` 执行 UPDATE 无事务，翻页变成 N 次写+fsync。回填只走事务版 `backfill_missing_usage_costs`。

**【高】4. skill ZIP 解压存在 zip-slip 缺口且阻塞 async** — skill.rs:2281-2311 `download_and_extract` 用 `file.name()` 手工 strip 后直接 join，未用 `enclosed_name()`（extract_local_zip:2704 用了）；整包读内存、zip/fs 阻塞跑在 async fn。改 `enclosed_name()` + `spawn_blocking`。

**【高】5. async 上下文跑阻塞子进程** — subscription.rs:133,483,784 macOS Keychain `security` 用 `std::process::Command::output()`（可能触发授权弹窗长阻塞），sync_protocol.rs:363 `hostname` 同理。改 `tokio::process::Command` 或 `spawn_blocking`。

**【中】6. Token 回同步三分支复制且吞错** — proxy.rs:658-869 Claude/Codex/Gemini 三段 ~200 行近乎相同，DB 写失败仅 warn 后返回 Ok，代理继续用旧 token 401。抽公共注入函数并上抛失败。

**【中】7. WebDAV 与 S3 同步层成对复制** — s3_auto_sync.rs 与 webdav_auto_sync.rs 约 280 行逐行相同；s3_sync.rs 与 webdav_sync.rs 编排平行副本。以 transport trait 泛化，保留 sync_protocol.rs 为唯一编排。

**【中】8. 多设备同步无并发防护（lost update）** — webdav_sync.rs:64-107、s3_sync.rs:49-90。upload 直接 PUT 覆盖，从不比对 `last_remote_etag`；两台设备并发会静默互吞配置。上传前 etag 比对。

**【中】9. 限额查询函数包列致索引失效** — usage_stats.rs:1384-1413 用 `date(datetime(created_at,...))` 包裹列做日/月过滤全表扫，`.unwrap_or(0.0)` 吞错（限额形同虚设）。预计算时间戳边界做范围条件。

**【中】10. session_usage_* 四文件复制粘贴** — 四文件重复同一骨架（mtime+行偏移扫描、DedupKey→定价→24 列 INSERT、模型归一化），`normalize_codex_model` 与 `strip_model_date_suffix` 逻辑重叠。抽 `SessionLogImporter` trait。

**【低】11. 排序更新逐行独立事务** — provider/mod.rs:2577-2587 一次拖拽 N 次写事务 + N 次 auto_sync 通知。包单事务。

**【低】12. proxy.rs 职责过载、三套备份/接管实现并存** — 实现约 2500 行混合 takeover 生命周期、live IO、token 同步、TOML 改写；backup/takeover 各有 bulk/strict/best_effort 三套平行路径（proxy.rs:1003/1055/1129/1192/1253）。拆子模块 + 枚举参数收敛。

### 总评
正确性意识强——takeover 备份/恢复的占位符防腐、SSOT 兜底、同步 hash 校验回滚都有详尽回归测试。主要风险：统计查询的相关子查询去重 + 读路径写库随日志量线性恶化（且持全局 DB 锁）；sync/async 边界散布 block_on 与阻塞 IO。重复问题（webdav/s3、session_usage 四件套、token 三分支）建议下次功能迭代前以 trait 收敛。

---

## 四、命令 / 配置 / 数据库层（commands/、*_config.rs、database/）

### 【高】

**1. opencode.json 写回有损：丢注释、整文件按字母重排，且无锁、无备份**
- `opencode_config.rs:88-148`（json5 读 / 排序后纯 JSON 写）；`config.rs:164-193`
- 任何一次 set_provider/remove_provider/add_plugin（含启动自动 live 同步）静默销毁用户注释并重排所有键；无进程内锁（hermes/openclaw 都有）、无写前备份。
- 建议：照搬 openclaw 的 json_five round-trip 文档模式或只替换子树；补锁与备份；至少去掉 sort_json_keys。

**2. settings.json 写入非原子，且 `update_settings` 在锁外落盘，存在丢更新竞态**
- `settings.rs:609-644`（truncate 直写，无 temp+rename）；`692-702`（先存盘再拿锁）；`584-606`（解析失败静默回退默认值）
- 全项目唯一不走 atomic_write 的高频写文件；崩溃时丢各 app 当前供应商指针、目录覆盖、WebDAV/S3 密码、迁移标记。`update_settings` 与持锁的 `set_current_provider` 并发会"切了又弹回"。
- 建议：改用 `crate::config::atomic_write`（0600）；先拿写锁、锁内合并+落盘，与 mutate_settings 统一。

**3. `atomic_write` 在 Windows 上有"先删后改名"的丢文件窗口，且全平台无 fsync**
- `config.rs:204-259`：Windows 先 remove_file 再 rename，两步间被杀/掉电目标文件彻底消失；写完只 flush() 无 sync_all()。所有 JSON/TOML/.env/备份写入汇聚此函数，影响七个工具的 live 配置。
- 建议：Windows 用 `fs::rename` 失败再退 ReplaceFileW（或 `tempfile::NamedTempFile::persist`）；rename 前 `sync_all()`。

### 【中】

**4. 五套 `*_config.rs` 写入策略各自演化** — 锁（hermes/openclaw 有，opencode/gemini/claude 无）、备份（hermes/openclaw 有）、冲突检测（仅 openclaw）、注释保留（openclaw/hermes）四维度完全不一致；openclaw 自身 set_provider 双读竞态残口（openclaw_config.rs:659-680）。抽统一写管线 `load → mutate → save{锁,外部变更检测,备份,atomic_write}`，五家以 codec 参数接入。

**5. misc.rs 4697 行应拆分** — 工具版本探测/安装引擎（~2300 行）+ 终端启动（三平台）+ 零散命令；文件内已重复：Linux 终端表两份（misc.rs:2814 与 3124）、macOS dispatch 两份（2574-2583 与 3091-3099）。拆 `commands/tool_lifecycle.rs` + `commands/terminal.rs`（一份终端表）。

**6. 同步 Tauri 命令在主线程执行 + 全库单把 Mutex<Connection>，UI 冻结面** — `commands/provider.rs:22,239` 非 async fn 在主线程做多文件 IO、block_on、MCP 全量同步；后台 usage 同步持锁批量插入时主线程等锁 → 界面卡死感。命令改 async + spawn_blocking；或多连接 + WAL 读写分离。

**7. 命令层三处业务逻辑下沉欠账** — `commands/provider.rs:505-739`（usage 查询编排 230 行）、`306-501`（Claude Desktop 导入路由推断 200 行）、`commands/config.rs:279-354`（common config 迁移编排）。分别移入 services。

**8. `sync_current_providers_live` 新建 `AppState::new(db)`，绕开受管 ProxyService 的 per-app 切换锁** — `commands/import_export.rs:63-76`：新 AppState 携带全新 SwitchLockManager、无 app_handle，与并发 switch_provider 完全不互斥，破坏"切换与接管串行化"。改持有 `State<AppState>` 传 Arc。

**9. `restore_from_backup` 先覆盖主库、后验证版本** — `database/backup.rs:581-628`，对照 import_sql 的"临时库先验证"正确顺序。备份来自更新版本时主库变成打不开的状态。先读 user_version 拒绝过新，或先恢复到临时库。

**10. Schema 演进双轨制** — `schema.rs:300-364` 七个 `let _ = ALTER TABLE`（吞掉磁盘满等真实错误）+ user_version 迁移并存，新增列必须两边都改；v11-v13 已是空迁移。收敛为"建表只建终态 + 旧库修复一律走版本化迁移"。

**11. Gemini settings.json 解析失败被静默当空对象整文件重写** — `gemini_config.rs:294-335`：用户手写多个逗号，下次切换 Gemini 供应商整个文件被替换成骨架，配置全丢且无备份。解析失败应报错，至少先备份 .bak。

**12. lib.rs `setup()` 全程同步阻塞主线程，含潜在整库 VACUUM** — `lib.rs:336-1123`；`database/mod.rs:96-159`（ensure_incremental_auto_vacuum 可能整库 VACUUM + rollup_and_prune(30)）；`lib.rs:544-786`（7 app live import、MCP 五源、prompt 六 app 串行）。老用户启动"双击没反应"。Database::init 只做打开+迁移，其余移 spawn_blocking 后台 + 事件通知前端。

**13. `open_provider_terminal` 把 API Key 明文写进 /tmp 且未设 0600** — `commands/misc.rs:2481-2536`：默认 644，多用户机器其他本地用户可读 Key（gemini_config.rs:179-187 已有 0600 范本）。`tempfile::Builder::permissions(0o600)`。

### 【低】

**14. dao 层 291 处 `.map_err(|e| AppError::Database(e.to_string()))` 纯样板** — error.rs 已有 `From<rusqlite::Error>`，大半可用 `?` 替代；`query_row + QueryReturnedNoRows → Option` 三态匹配重复 10+ 次（rusqlite 自带 OptionalExtension）。

**15. `save_provider` 的 UPDATE 分支静默丢弃 `meta.custom_endpoints`** — `database/dao/providers.rs:180-278`：endpoints 只有 INSERT 分支写入，UPDATE 完全不动。WebDAV/deeplink 批量导入易踩。UPDATE 内 delete+reinsert，或类型层面剥离。

**16. `get_all_providers` N+1 查询 + 解析失败静默降级 Null** — `providers.rs:20-109`：损坏的 settings_config 降级 Null 后续会写回数据库，把可修复损坏固化为数据丢失。JOIN 一次取出；解析失败记日志保留原文。

**17. `write_codex_live_atomic` 读取 `_old_config` 死代码；回滚失败被 `let _ =` 吞掉** — `codex_config.rs:101-105,120-127`。删除死读；回滚失败 log::error 并并入返回错误。

**18. `futures::executor::block_on` 依赖"async fn 实为同步"的隐性契约** — `services/provider/mod.rs:1969-2014`、`tray.rs:503,540`、`claude_desktop_config.rs:863-877`、`live.rs:939-960`。要么 DAO 改回同步 fn，要么把约束写成显式文档+lint。

### 整体评价
整体素质明显高于同类社区项目（SAVEPOINT 迁移+备份+过新拒绝、SQL 导入临时库验证、快照回滚、乐观冲突检测、决策注释、真实回归测试）。结构性债务两点：一是"同一件事的第 N 个实现各自演化"（五套配置写入、两份终端表、双轨 schema）；二是边界纪律（misc.rs 养了 2300 行引擎）。最优先修三处"静默破坏用户文件"路径（opencode 丢注释、gemini 吞解析错误重写、settings.json 非原子写），与项目"保护用户 live 配置"的核心卖点直接冲突，修复成本都不高。

---

## 五、React 前端（src/）

### 批 1：App.tsx + hooks

**1.【高】src/App.tsx:250-265 + src/hooks/useProxyStatus.ts:24-31 — 代理运行时全 App 树每 2 秒重渲染**
`useProxyStatus` 以 `refetchInterval: 2000` 轮询，`ProxyStatus` 含 uptime/total_requests 等每次必变字段，App 顶层消费导致整个 App（header、ProviderList、全部 ProviderCard——均无 React.memo）每 2s 全量重渲染；另有 5 个组件各自订阅同步重渲。
改法：App 用 `select` 切片只取 running/active_targets；易变字段拆独立查询仅 ProxyPanel 订阅；ProviderCard 加 React.memo。

**2.【中】src/App.tsx:163-1610 — god component**：14 个 view 的 switch、窗控、env 冲突、CRUD 编排、多渠道工具栏内联。拆 `<AppHeader/>`、`useWindowState()`、`useEnvConflicts()`；view 查表渲染。

**3.【中】src/App.tsx:184-209 — visibleApps 兜底字面量每次渲染新建并作为 effect 依赖**，effect 每次渲染都跑。提取模块级常量或 useMemo。

**4.【中】src/hooks/useSettings.ts:175-299 vs 303-478 — autoSaveSettings 与 saveSettings 约 120 行复制粘贴**，已出现行为漂移（autoSave 不处理目录变更回写 live）。抽 `persistSettings(merged, opts)`。

**5.【中】useSettings.ts:181-202,421-441 + useSettingsForm.ts:121-126 — hermesConfigDir 被系统性遗漏**（不 trim、变更不触发 live 回写；resetSettings 却包含 hermes）。用 `DIRECTORY_KEY_TO_SETTINGS_FIELD` 表驱动消灭手写清单。

**6.【中】useSettingsForm.ts:103-132 + App.tsx:373-403 — 后台同步事件会清掉用户未保存的设置表单**：sync 状态事件 invalidate ["settings"] → refetch 新 data 无条件覆盖编辑中表单。加 dirty 检查或只初始化一次。

### 批 2：ProviderForm 及各 *FormFields

**7.【高】ProviderForm.tsx:243-2309 — 七应用合一的 2300 行表单，无关 app 的状态机全量实例化**：useCodexConfigState/useGeminiConfigState/useOmoModelSource/useOpenclawFormState/useHermesFormState 不论 appId 一律执行；1370-1448 连续 6 次 `useApiKeyLink` 各自 watch——实际只用 1 个；根级 watch 任意击键重渲整棵树。
改法：useApiKeyLink 传真实 appId 一次调用；按 appId 拆子表单组件让 hooks 随组件挂载；watch 下沉 useWatch。

**8.【中】ProviderForm.tsx:1675-1880 + 956-1037 — providerKey 输入块与校验三段复制**（opencode/openclaw/hermes 各 ~70 行 JSX 仅 i18n 键不同；正则内联重复 9 次；四连校验复制 3 份）。抽 `<ProviderKeyField/>` + `validateProviderKey` + 模块常量。

**9.【中】七个表单组件 — "获取模型"逻辑七处复制**（fetchedModels/isFetching + then/catch/finally 同构，空结果文案已分叉）。抽 `useFetchModels({baseUrl, apiKey, ...})`。

**10.【中】ProviderForm.tsx:429,441,457,633,646,1889,2149,2173 — form.getValues 作为渲染期 props 非响应式**，依赖其它 state 恰好同步变化才正常。展示值改 useWatch；已配 getConfig 回调的删多余快照入参。

**11.【低】ProviderForm.tsx:1253-1293 — 清空自定义端点的逻辑不可达**（包在 `length > 0` 分支内，needsClearEndpoints 恒 false；编辑模式整段跳过）。把 clear 判断移出守卫。

**12.【低】ProviderForm.tsx:267-277 — 保存 commonConfigConfirmed 只剥 webdavSync 不剥 s3Sync**，把查询快照里的 s3Sync 原样写回，可能覆盖后台刚更新的同步状态。与 useSettings 对齐同剥两个。

### 批 3：WebdavSyncSection / UsageScriptModal / SessionManagerPage

**13.【高】WebdavSyncSection.tsx:234-1867 — WebDAV 与 S3 两套逻辑整镜像复制（约 900 行）**：28 个 useState 一一对应，六个处理器各两份（472-650 vs 697-861），确认对话框 JSX 两份。抽 `useSyncBackend({api, i18nPrefix})` + `<SyncConfirmDialog/>`；新增第三种后端成本从 900 行降到一个配置对象。

**14.【中】WebdavSyncSection.tsx:517,580,640,736,796,851,909 — 无参 invalidateQueries() 触发全量 refetch 风暴**（失效全应用所有查询并发重拉几十个 Tauri 命令）。改显式 queryKey 列表。

**15.【中】SessionManagerPage.tsx:788-810 + SessionItem.tsx:32 — 会话左侧列表不虚拟化、不 memo、搜索不防抖**：每击键全部会话行（数千条）同步重渲 + FlexSearch 查询。接入 useDebouncedValue + React.memo + useVirtualizer。

**16.【低】UsageScriptModal.tsx:147-275,194,277 — settingsConfig 全程 as any + 模板/凭据每渲染重算**。建窄类型判别联合 + 纯函数 getCredentials + useMemo。

### 批 4：providers 对话框/列表

**17.【中】EditProviderDialog.tsx:360-364 — 编辑 hermes 供应商时 providerKey 改名被静默丢弃**：nextProviderId 只认 opencode/openclaw，hermes 漏掉（Add 路径三个都处理）。补条件，或引入 `isAdditiveApp(appId)` 谓词统一三处。

### 批 5：跨组件模式

**18.【中】FirstRunNoticeDialog.tsx:29-30、UsageScriptModal.tsx:427、ProviderList.tsx:239-240、ProviderForm.tsx:270-271 — "确认 flag" 四处复制同一危险的整包回写**：取 settings 快照 → 剥 webdavSync（都没剥 s3Sync）→ save({...rest, xxxConfirmed}) → invalidate。基于过期快照的 RMW，并发覆盖丢 flag/丢设置。后端加 `set_settings_flag(key)` 局部更新命令，四处替换（发现 12 一并解决）。

### 总评
整体架构健康：invoke 收敛在 lib/api 22 个模块、Query key 管理与小 hooks 质量高、zod 前置校验、消息侧虚拟滚动、无 listen 泄漏。债务三类：一是"按渠道复制扩展"惯性（ProviderForm 七合一、WebDAV/S3 镜像、七份 fetch-models、四份确认 flag），已造成 hermes 至少 4 处系统性漏掉的真 bug；二是重渲染面（2s 轮询打穿全树、会话列表、根级 watch）；三是设置写回模型（整包 save + 无参 invalidate 既有竞态又有风暴）。优先：useProxyStatus select 切片 + ProviderCard memo，抽 ProviderKeyField/useFetchModels/useSyncBackend，确认 flag 改后端局部更新。

---

## 六、优先级路线图（汇总）

### P0 正确性（建议尽快修，成本低收益高）
1. failover 跳过 provider 漏洞（forwarder.rs pending 机制）
2. settings.json 原子写 + 锁内落盘（settings.rs）
3. atomic_write Windows 丢文件窗口 + fsync（config.rs）
4. opencode.json 有损写回 / gemini settings 解析失败覆盖（两处静默破坏用户文件）
5. 流式 error 事件被吞（streaming_responses/streaming/streaming_gemini 统一 detect_upstream_error）
6. Gemini usage 缓存 token 双计（计费失真）
7. hermes 系统性遗漏 4 处（useSettings 目录清单 + EditProviderDialog providerKey）
8. is_key_scoped_error 过宽误判 / zip-slip / /tmp 明文 Key 0600
9. 官方 reasoning 事件名缺失（streaming_responses，自产流喂自己都丢 thinking）

### P1 性能热路径
1. SQLite：WAL + busy_timeout + 热路径配置缓存 + 记账异步化（响应前 2-3 次同步写挪后台）
2. anthropic 路径连接复用（每请求 TLS 握手是最大固定开销）
3. 读路径写库（usage_stats 回填）+ 相关子查询去重 + 函数包列改范围条件
4. 前端：useProxyStatus select 切片 + ProviderCard memo + 会话列表防抖/虚拟化
5. 启动：VACUUM/rollup/批量导入移出 setup 主线程
6. 转换层：全量递归遍历 input、历史输出重复 canonicalize、body 三次深拷贝

### P2 重复收敛（防"改一处漏四处"）
1. Rust：NormalizedUsage、AnthropicSseEmitter、merge_system_to_head、统一 schema sanitizer、配置写入统一管线（锁/备份/原子/冲突检测）、webdav/s3 transport trait、session_usage importer trait、token 回同步、整流记账 helper、终端表
2. 前端：useSyncBackend、ProviderKeyField、useFetchModels、isAdditiveApp、set_settings_flag 局部更新、表驱动目录清单

### P3 结构与体积
1. 拆分：forwarder.rs（~5000）、services/proxy.rs（~5600）、commands/misc.rs（~4700）、App.tsx（1610）、ProviderForm.tsx（2319）、WebdavSyncSection（1867）
2. 前端体积：React.lazy 按 view 分包 + manualChunks（recharts/codemirror 拆出）+ i18n 按语言懒加载 + 压缩 1.1MB/436KB 图标
3. dao 样板 291 处 map_err → From + OptionalExtension
