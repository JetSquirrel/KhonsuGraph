# Issue 手工验证报告 — 2026-11-03

本轮已实际操作 Windows 桌面窗口，并补测 Windows release 命令行性能。没有修改产品代码，没有关闭或更新 GitHub issue。本报告记录已获得的证据，不将未执行的验收项记为通过。

## 结论与 issue 建议

| Issue | 本轮结论 | 建议 |
| --- | --- | --- |
| #33 Evidence Inspector | 节点/关系证据、来源、稳定 ID、复制 JSON、缺失元数据及导出脱敏通过；发现界面脱敏提示与普通属性显示不一致 | 完整 AWS API 响应不应成为本 issue 的阻塞项；新发现的界面问题应先修复或明确独立跟踪，再关闭 |
| #34 保存与恢复调查 | 保存、重新打开、进程重启后手动打开、路径和筛选恢复、图变化警告通过 | dashboard/布局仍属已知缺口；Windows 打开调查入口还需处理。完成范围拆分并记录后再关闭，不宜直接宣称全部范围完成 |
| #35 AWS exposure | 正例、反例、数据不足例及路径/证据联动通过 | 继续开放：本轮不能替代新工程师本人 ≤10 分钟的上手验收 |
| #36 测试与性能门槛 | 补齐本机 Windows release 小图、大图的部分性能基线；小图界面操作未观察到卡死 | 继续开放：macOS、定量 UI 延迟/FPS、负载下取消响应尚未测量 |
| #37 snapshots/diff | 通过保存调查的图变化警告验证；本轮未验收 diff 视图及路径/exposure 差异 | 继续开放 |
| #31、#40、#41、#42 | 本轮没有重新核查这些 issue 的实现或验收 | 保持现状；本报告不提供关闭依据 |

## 环境和方法

- Windows 11，内核 build 26200；AMD Ryzen 5 6600H，系统可用物理内存约 31.19 GiB。
- CLI release：`cargo build --release --locked --workspace` 成功，构建用时 8m 37s；提交 `10d29e2cb94d5eb772f7574cb8dfc8927d2781d5`。
- GUI：已有 `target/debug/khonsugraph.exe`，界面版本 KhonsuGraph 0.2.0 / HawDB 0.24.0。GUI 观察属于 debug 构建，不能作为 release UI 性能数据。
- GUI 主场景：用户已打开的 `E:\graphs\aws-demo`，27 节点、45 关系。
- CLI 测试图、附加异常场景和原始结果均放在 `scratch/manual-20261103/`；修改图数据只发生在独立测试图中。
- AWS 数据来自仓库离线 fixture，没有访问真实 AWS 账户。
- GUI 使用实际鼠标、键盘、窗口截图及可访问文本检查；复制出的 JSON 经粘贴后解析验证，没有将其作为查询执行。

最初的 release 构建和图访问在沙箱内失败；沙箱外重试成功。`zstd/lib` 实际存在，不能把最初的错误归因为依赖源码缺失。

## 已完成的手工验证

| 场景 | 操作与结果 |
| --- | --- |
| 图查询与显示 | 执行 `MATCH (n)-[r]->(m) RETURN n, r, m LIMIT 1000`，得到 45 行、27 节点；表格和图视图均可显示。界面查询计时为 1 ms，这不是端到端 UI 延迟。 |
| 实例证据 | 选择 `web-1`，显示稳定 Evidence ID、来源 `shop`、插件 `aws@0.2.0`、记录时间、同步时间和完整同步信息。 |
| 关系证据 | 选择 Internet 到 web 安全组的 `ALLOWS` 关系，显示稳定 ID、两个端点、来源、快照 `shop#1`、explicit assertion，以及 `ports=443`、`protocol=tcp`、rule。 |
| 关系 JSON 复制 | 点击 Copy evidence as JSON，粘贴到临时编辑页并解析成功；`stable=true`，属性和所选关系一致。 |
| 正例 | `web-1` 为 `potential_exposure`，规则 `aws.exposure.direct-instance`；显示运行状态、公网地址、TCP/443 入站规则、互联网路由及对应 AWS 字段名。 |
| 路径与假设 | Show path on canvas 显示 `Internet -[ALLOWS]-> web <-[PROTECTED_BY]- web-1`；显示未检查项，并明确不代表已证明可达。 |
| 反例 | `batch-1` 开放 TCP/22，但没有公网 IPv4/IPv6，显示 Not indicated by this rule，而非已暴露或可达。 |
| 数据不足例 | `legacy-1` 显示 Insufficient data；具体指出没有记录子网路由表或 VPC 主路由表，路由未知。 |
| 保存调查 | GUI 保存 `web-ui.investigation` 成功；文件包含原查询、稳定种子 ID、双向、2 跳、`ALLOWS`/`PROTECTED_BY`、确切路径及 pinned run。 |
| 同进程重新打开 | 通过 `khonsugraph open .../web-ui.investigation` 重新打开，原查询、3 节点/2 关系路径和筛选参数恢复。 |
| 重启后恢复 | 退出应用，确认窗口消失；再次打开调查后获得新的窗口句柄，路径、双向、2 跳和关系类型筛选仍正确。此项验证的是重启后手动打开，不是自动恢复调查。 |
| 图变化警告 | 在独立 exposure 测试图将 `web-1` 改名后，CLI 返回 `graph_state=changed`、`path_same=true`；GUI 在画布和 Inspector 显示图内容已变化、结果可能不同的黄色警告。 |
| 缺失元数据 | 独立创建无来源 Host 节点，GUI 的来源、插件、时间、覆盖情况和 assertion 显示 Not supplied；提示内部 ID 可能随重建变化。CLI 对应字段为 `unknown`。 |
| 导出脱敏 | 无来源节点含模拟 `api_token`。GUI Copy evidence 和 CLI evidence 都输出 `[redacted]`，`redacted` 列表包含 `api_token`，JSON 中没有模拟 token 原值。普通属性列表显示问题见下文。 |

CLI exposure 总计为 3 个 `potential_exposure`、1 个 `insufficient_data`、3 个 `not_indicated`，与 walkthrough 一致；CLI 保存/恢复也确认 `graph_state=unchanged`、`path_same=true`。

## 新发现的问题

### 1. Inspector 脱敏提示与下方普通属性显示不一致

复现步骤：

1. 在独立图创建 `(:Host {name: 'manual-legacy', api_token: 'SYNTHETIC_NOT_A_SECRET'})`。
2. 在画布选择该节点，滚动到 Evidence 底部。
3. 界面提示 `Masked as possible secrets: api_token`，但紧接着的普通属性列表仍显示 `api_token` 原值。
4. Copy evidence as JSON 中该值确实为 `[redacted]`。

影响：用户可能把“已脱敏”理解为整个 Inspector 都已隐藏敏感值，实际截图或屏幕共享仍会带出原值。本轮只使用模拟值，没有暴露真实凭证。

建议：让普通属性显示遵循同一脱敏规则，或明确提示仅证据导出已脱敏。此问题不等同于“导出泄密”；本轮 GUI 和 CLI 导出检查均通过。

相关实现：`app/src/ui/canvas.rs` 的 Evidence 渲染及所选节点属性显示。

### 2. Windows 可见菜单缺少文档所述的打开调查入口

`docs/investigations.md` 指示使用 File > Open investigation…。本次 Windows 窗口可见的 Graph 菜单只有打开图、新建图、只读重开、添加来源、关闭图，没有打开调查。代码中定义了原生 File 菜单动作，但可见 Graph 菜单没有对应项，也没有为 OpenInvestigation 设置快捷键。

本轮通过 CLI `khonsugraph open <file.investigation>` 成功恢复。建议补上可发现的 Windows 菜单入口，并使文档与 Windows 实际操作一致。

相关实现：`app/src/ui/title_bar.rs` 的 `render_graph_menu`，以及 `app/src/ui/mod.rs` 的菜单和键绑定。

## Windows release 性能基线

时间为整个 CLI 进程耗时，包含启动和打开图。除明确标注外均运行 3 次，表中为中位数。内存为主进程峰值 working set，约每 5 ms 采样；不包括插件子进程。磁盘缓存未清空，这些不是冷启动基准，也不是性能承诺。

### AWS exposure fixture：27 节点、45 关系

| 操作 | 中位耗时 | 中位峰值内存 |
| --- | ---: | ---: |
| `analyze summary` | 175 ms | 22.45 MiB |
| `check exposure.dash` | 179 ms | 24.74 MiB |
| 绑定 Internet/web-1 端点的 `shortestPath` | 185 ms | 22.33 MiB |
| `plugin analyze aws exposure` | 502 ms | 24.22 MiB |
| `explore` 双向、2 跳、两种关系类型 | 202 ms | 23.96 MiB |
| `evidence` 查询 web-1 | 203 ms | 22.12 MiB |

### Audit：5,881 节点、35,592 关系

| 操作 | 中位耗时 | 中位峰值内存 |
| --- | ---: | ---: |
| `analyze summary` | 2.64 s | 178.46 MiB |
| 节点 `count(n)` | 1.20 s | 131.27 MiB |
| `MATCH (a)-[]->(b)-[]->(c) RETURN count(*) AS c` | 5.06 s | 132.55 MiB |
| `analyze pagerank --type TRUSTS --top 10` | 2.21 s | 169.86 MiB |
| `check trust_network.dash` | 1.55 s | 138.53 MiB |

从本地 CSV 同步到新 audit 图另测 **1 次**：4.24 s、213.45 MiB；不是三次中位数。

本次两跳查询返回 2,301,858；旧文档记录的两跳结果数为 1,948,728，不能直接用两组时间宣称同一查询的加速比例。AWS 本次也是 27/45 的 exposure fixture，旧基线是 16/24 的 shop fixture。

已有 audit 图曾返回 `old_format`；没有覆盖或重建旧图，而是从 CSV 创建独立测试图后测量。

## 尚未覆盖或不能据此通过的项目

- 新工程师本人按文档在 10 分钟内完成调查。工具操作时间、环境准备和已有项目知识均使本轮不适合充当该用户研究。
- macOS 构建与性能。
- UI 输入到可见反馈的定量延迟、FPS、繁忙时响应和取消延迟。本轮只验证小图点击、滚动、切换、分析与重开能够完成；截图/自动化工具耗时不计作 UI 延迟。
- dashboard、视图状态和节点位置恢复：仓库文档明确列为当前限制，本轮未对其做新的完整验收。
- diff 视图、路径差异、exposure 差异及 #40/#41/#42。
- 全量 CI、格式版本异常和所有损坏输入场景。本轮的命令行验证不替代完整自动化测试套件。

## 证据文件

本机完整原始结果和验证脚本位于 `scratch/manual-20261103/`（该目录被 Git 忽略）：

- `release-baseline.json`、`release-audit-baseline.json`：逐次耗时、峰值内存、命令。
- `verify.py`、`audit.py`、`edgecases.py`：本轮执行的命令记录；包含创建/修改测试图步骤，重复执行应使用新的测试目录。
- `web-ui.investigation`：实际由 GUI 保存的调查。
- `reopen-0.json`、`changed-reopen.json`：未变化/已变化的恢复输出。
- `legacy-evidence.json`：无来源及模拟敏感字段的脱敏输出。
- `exposure-0.json`、`explore-0.json`、`evidence-0.json`：规则、路径和证据结果。

随报告保存的性能数据副本见 [小图原始基线](validation/2026-11-03/release-baseline.json) 和 [audit 原始基线](validation/2026-11-03/release-audit-baseline.json)。测试结束时已将窗口打开到原 AWS 图的保存调查。
