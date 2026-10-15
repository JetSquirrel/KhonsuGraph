//! UI string internationalization (zh / en), hand-rolled with no extra
//! dependencies.
//!
//! Every user-visible string lives in the `STRINGS` table below as
//! `(key, zh, en)`. Keys describe stable intent (`dialog.export.title`),
//! never a source-language sentence; each locale gets a full-sentence
//! template with sequential `{}` placeholders for dynamic values.
//!
//! Usage in views: `i18n::tr("sidebar.tab.schema")` for plain strings,
//! `i18n::trf("status_bar.last_query", &[&elapsed, &rows, &cols])` for
//! templates (extra `{}` placeholders are left untouched).
//!
//! Language switching (for the title_bar language-toggle UI):
//! call `i18n::set_language(Language::En)` — it updates the global and
//! persists the choice to the settings file (blocking write, fine from a
//! click handler) — then call `cx.refresh_windows()` so every view
//! re-renders in the new language without per-view subscriptions.

use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Language {
    Zh,
    En,
}

impl Language {
    pub fn code(&self) -> &'static str {
        match self {
            Language::Zh => "zh",
            Language::En => "en",
        }
    }

    /// Unknown codes fall back to Chinese, the language the PRD was written in.
    pub fn from_code(code: &str) -> Language {
        match code {
            "en" => Language::En,
            _ => Language::Zh,
        }
    }
}

static CURRENT: RwLock<Language> = RwLock::new(Language::Zh);

/// What joins the parts of a list in running text: `, ` in English, the
/// full-width `，` in Chinese. The interface writes lists as text, never
/// with a middle dot.
pub fn sep() -> &'static str {
    match current() {
        Language::Zh => "，",
        Language::En => ", ",
    }
}

/// `a` and `b` as one line. In English a part that is its own sentence
/// ("Click to filter …") follows a full stop, not a comma.
pub fn join(a: &str, b: &str) -> String {
    join_in(current(), a, b)
}

fn join_in(language: Language, a: &str, b: &str) -> String {
    let a = a.trim_end_matches(['.', ',', '，', '。']);
    let between = match language {
        Language::En if b.starts_with(char::is_uppercase) => ". ",
        Language::En => ", ",
        Language::Zh => "，",
    };
    format!("{a}{between}{b}")
}

/// `parts` as one line, joined as [`join`] joins two; `None` when empty.
pub fn join_all<S: AsRef<str>>(parts: &[S]) -> Option<String> {
    let mut parts = parts.iter().map(AsRef::as_ref);
    let first = parts.next()?.to_string();
    Some(parts.fold(first, |line, part| join(&line, part)))
}

pub fn current() -> Language {
    *CURRENT.read().unwrap_or_else(|e| e.into_inner())
}

pub fn set_current(lang: Language) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = lang;
}

/// Language to use at startup: the persisted `language` setting when the
/// settings file is reachable, otherwise the detected system locale. Never
/// persists anything — that only happens on an explicit user switch.
pub fn initial_language() -> Language {
    if let Ok(Some(code)) = crate::prefs::get("language") {
        return Language::from_code(&code);
    }
    detect_system_language()
}

/// Best-effort system locale detection: LC_ALL / LANG containing "zh"
/// (`zh_CN.UTF-8`, `zh-Hans`, …) means Chinese, anything else English.
pub fn detect_system_language() -> Language {
    let locale = std::env::var("LC_ALL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var("LANG").ok())
        .unwrap_or_default()
        .to_lowercase();
    if locale.contains("zh") {
        Language::Zh
    } else {
        Language::En
    }
}

/// Switch the UI language and persist the choice. Blocking (writes to the
/// settings file); callers on the UI thread should follow with
/// `cx.refresh_windows()` to repaint all views in the new language.
pub fn set_language(lang: Language) {
    set_current(lang);
    if let Err(e) = crate::prefs::set("language", lang.code()) {
        tracing::warn!("Failed to persist language setting: {e}");
    }
}

static ZH: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| STRINGS.iter().map(|(key, zh, _)| (*key, *zh)).collect());
static EN: LazyLock<HashMap<&'static str, &'static str>> =
    LazyLock::new(|| STRINGS.iter().map(|(key, _, en)| (*key, *en)).collect());

/// Translate `key` in the current language, falling back to the zh value,
/// then to the key itself. Never panics on a missing key.
pub fn tr(key: &'static str) -> &'static str {
    translate(current(), key)
}

fn translate(lang: Language, key: &'static str) -> &'static str {
    let map = match lang {
        Language::Zh => &*ZH,
        Language::En => &*EN,
    };
    map.get(key).or_else(|| ZH.get(key)).copied().unwrap_or(key)
}

/// Translate `key` and substitute sequential `{}` placeholders with `args`
/// in order. Leftover `{}` placeholders stay as-is; extra args are ignored.
pub fn trf(key: &'static str, args: &[&str]) -> String {
    format_template(tr(key), args)
}

/// [`trf`] in a given language rather than the UI's: the CLI speaks English
/// whatever the window does.
pub fn trf_in(lang: Language, key: &'static str, args: &[&str]) -> String {
    format_template(translate(lang, key), args)
}

fn format_template(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    for arg in args {
        match rest.split_once("{}") {
            Some((before, after)) => {
                out.push_str(before);
                out.push_str(arg);
                rest = after;
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

/// `(key, zh, en)` for every user-visible string, grouped by view.
static STRINGS: &[(&str, &str, &str)] = &[
    // ── Common ──────────────────────────────────────────────────────────
    ("common.cancel", "取消", "Cancel"),
    ("common.close", "关闭", "Close"),
    ("app.tagline", "HawDB 图数据的夜行者工作台", "A night traveler's workbench for HawDB graphs"),

    // ── sidebar ─────────────────────────────────────────────────────────
    ("sidebar.tab.schema", "结构", "Schema"),
    ("sidebar.tab.history", "查询历史", "History"),
    ("sidebar.tab.sources", "数据源", "Sources"),
    ("sidebar.labels", "节点标签", "Labels"),
    ("sidebar.relationship_types", "关系类型", "Relationship types"),
    ("sidebar.tables", "表", "Tables"),
    ("sidebar.loading", "正在读取结构…", "Loading schema…"),
    ("sidebar.no_graph", "打开或新建一个图后，这里会显示它的结构。", "Open or create a graph to see its schema here."),
    ("sidebar.refresh_schema", "刷新结构", "Refresh schema"),
    ("sidebar.sample_query", "查询示例数据", "Query a sample"),
    ("sidebar.schema.empty", "图中还没有数据", "The graph has no data yet"),
    ("sidebar.history.empty", "还没有查询记录", "No queries yet"),
    ("sidebar.history.clear", "清空历史", "Clear history"),
    ("sidebar.expand", "展开侧边栏", "Show sidebar"),
    ("sidebar.collapse", "收起侧边栏", "Hide sidebar"),

    // ── title bar ───────────────────────────────────────────────────────
    ("title_bar.graph", "图", "Graph"),
    ("title_bar.open_graph", "打开图…", "Open Graph…"),
    ("title_bar.new_graph", "新建图…", "New Graph…"),
    ("title_bar.close_graph", "关闭图", "Close Graph"),
    ("title_bar.reopen_writable", "以可写方式重新打开", "Reopen for Writing"),
    ("title_bar.reopen_read_only", "以只读方式重新打开", "Reopen Read-Only"),
    ("title_bar.read_only", "只读", "Read-only"),
    ("title_bar.toggle_language", "切换语言", "Switch language"),
    ("title_bar.toggle_theme", "切换深浅色", "Toggle light / dark"),
    ("title_bar.ui_size", "界面大小", "Interface size"),
    ("ui_size.small", "小", "Small"),
    ("ui_size.default", "默认", "Default"),
    ("ui_size.large", "大", "Large"),
    ("ui_size.xlarge", "特大", "Extra large"),

    // ── menus ───────────────────────────────────────────────────────────
    ("menu.file", "文件", "File"),
    ("menu.edit", "编辑", "Edit"),
    ("menu.query", "查询", "Query"),
    ("menu.view", "视图", "View"),
    ("menu.open_logs", "打开日志目录", "Open Logs Folder"),
    ("menu.quit", "退出 KhonsuGraph", "Quit KhonsuGraph"),
    ("menu.close_tab", "关闭标签页", "Close Tab"),
    ("menu.next_tab", "下一个标签页", "Next Tab"),
    ("menu.previous_tab", "上一个标签页", "Previous Tab"),
    ("menu.undo", "撤销", "Undo"),
    ("menu.redo", "重做", "Redo"),
    ("menu.cut", "剪切", "Cut"),
    ("menu.copy", "复制", "Copy"),
    ("menu.paste", "粘贴", "Paste"),
    ("menu.select_all", "全选", "Select All"),
    ("menu.toggle_sidebar", "显示/隐藏侧边栏", "Toggle Sidebar"),
    ("menu.zoom_in", "放大界面", "Zoom In"),
    ("menu.zoom_out", "缩小界面", "Zoom Out"),
    ("menu.zoom_reset", "实际大小", "Actual Size"),

    // ── opening graphs ──────────────────────────────────────────────────
    ("notify.open.failed_title", "无法打开图", "Couldn't open the graph"),
    ("notify.open.locked_title", "图正被占用", "The graph is in use"),
    ("notify.open.failed", "打开失败：{}", "Opening failed: {}"),
    ("notify.open.locked", "另一个进程正在写这个图：{}", "Another process is writing to this graph: {}"),
    ("notify.open.not_a_graph", "{} 不是 HawDB 图目录", "{} is not a HawDB graph directory"),
    ("notify.open.not_empty", "{} 不是空目录，不能在这里新建图", "{} is not empty; a new graph needs an empty folder"),
    ("error.no_graph", "还没有打开图", "No graph is open"),

    // ── welcome ─────────────────────────────────────────────────────────
    ("welcome.recent", "最近打开", "Recent"),
    ("welcome.no_recent", "还没有打开过图", "No recent graphs"),
    ("welcome.read_only", "只读打开", "Open read-only"),
    ("welcome.forget", "从列表移除", "Remove from list"),
    ("welcome.privacy", "数据只保存在本机，不会上传。", "Your data stays on this computer and is never uploaded."),

    // ── workspace ───────────────────────────────────────────────────────
    ("workspace.run", "运行", "Run"),
    ("workspace.explain", "执行计划", "Explain"),
    ("workspace.explain.tooltip", "查看 HawDB 为这条查询选择的执行计划", "Show the plan HawDB chooses for this query"),
    ("workspace.selection_hint", "选中部分文本时只运行选中内容", "With text selected, only the selection runs"),
    ("workspace.add_tab", "新建查询", "New query"),
    ("workspace.new_cypher", "新建 Cypher 查询", "New Cypher Query"),
    ("workspace.new_sql", "新建 SQL 查询", "New SQL Query"),
    ("workspace.untitled", "未命名", "Untitled"),

    // ── results ─────────────────────────────────────────────────────────
    ("results.table", "表格", "Table"),
    ("results.graph", "图", "Graph"),
    ("results.plan", "执行计划", "Plan"),
    ("results.rows", "{} 行", "{} rows"),
    ("results.rows_truncated", "显示 {} / {} 行", "{} of {} rows"),
    ("results.truncated", "只显示前 {} 行", "Showing the first {} rows"),
    ("results.empty", "运行查询后，结果显示在这里", "Run a query to see results here"),
    ("results.error_at", "第 {} 行，第 {} 列", "Line {}, column {}"),
    ("results.error_line", "第 {} 行", "Line {}"),
    ("results.export_csv", "导出 CSV", "Export CSV"),
    ("results.exported", "已导出到 {}", "Exported to {}"),
    ("results.filter", "筛选结果…", "Filter results…"),
    ("results.copy_row", "复制此行", "Copy row"),
    ("results.copied", "已复制", "Copied"),

    // ── graph canvas ────────────────────────────────────────────────────
    ("canvas.fit", "适配", "Fit"),
    ("canvas.relayout", "重新布局", "Re-layout"),
    ("canvas.expand", "展开邻居", "Expand neighbors"),
    ("canvas.hint", "双击节点展开邻居，右侧查看属性", "Double-click a node to expand it; select one to see its properties"),
    ("canvas.truncated", "显示 {} / {} 个节点", "Showing {} of {} nodes"),
    ("canvas.limit", "画布最多显示 {} 个节点，没有展开", "The canvas holds at most {} nodes; not expanded"),

    // ── status bar ──────────────────────────────────────────────────────
    ("status_bar.opening", "正在打开…", "Opening…"),
    ("status_bar.no_graph", "没有打开图", "No graph open"),
    ("status_bar.counts", "{} 个节点，{} 条关系", "{} nodes, {} relationships"),
    ("status_bar.last_query", "上次查询 {}，{} 行", "Last query {}, {} rows"),
    ("status_bar.last_query_graph", "，{} 个节点，{} 条关系", ", {} nodes, {} relationships"),
    ("status_bar.syncing", "正在同步 {}…", "Syncing {}…"),
    ("time.seconds", "{} 秒前", "{}s ago"),
    ("time.minutes", "{} 分钟前", "{}m ago"),
    ("time.hours", "{} 小时前", "{}h ago"),

    // ── data sources ────────────────────────────────────────────────────
    ("empty_graph.title", "这个图还是空的", "This graph is empty"),
    ("empty_graph.body", "添加一个数据源导入数据，或者直接用 Cypher 写入。", "Add a data source to import data, or write some with Cypher."),
    ("sources.add", "添加数据源…", "Add Data Source…"),
    ("sources.add_title", "添加 {} 数据源", "Add {} source"),
    ("sources.edit_title", "编辑 {}", "Edit {}"),
    ("sources.edit", "编辑", "Edit"),
    ("sources.empty", "还没有数据源", "No data sources yet"),
    ("sources.choose_plugin", "选择数据源类型", "Choose a source type"),
    ("sources.name", "名称", "Name"),
    ("sources.browse", "浏览…", "Browse…"),
    ("sources.required", "请填写 {}", "{} is required"),
    ("sources.not_a_number", "{} 需要是数字", "{} must be a number"),
    ("sources.secret_stored", "已保存在钥匙串中，留空则不修改", "Stored in the keychain; leave empty to keep it"),
    ("sources.keychain_failed", "无法写入钥匙串：{}", "Couldn't save to the keychain: {}"),
    ("sources.test", "测试连接", "Test"),
    ("sources.testing", "正在测试…", "Testing…"),
    ("sources.preview", "预览", "Preview"),
    ("sources.previewing", "正在预览…", "Previewing…"),
    ("sources.preview_title", "将导入 {} 个节点，{} 条关系", "Would import {} nodes, {} relationships"),
    ("sources.samples", "示例", "Samples"),
    ("sources.import", "导入", "Import"),
    ("sources.sync", "同步", "Sync"),
    ("sources.syncing", "正在同步…", "Syncing…"),
    ("sources.stop", "停止", "Stop"),
    ("sources.progress_counts", "已读取 {} 个节点，{} 条关系", "Read {} nodes, {} relationships"),
    ("sources.done", "已写入 {} 个节点，{} 条关系", "Wrote {} nodes, {} relationships"),
    ("sources.done_stale", "，移除 {} 条过期数据", ", removed {} stale"),
    ("sources.done_skipped", "，跳过 {} 条缺少端点的关系", ", skipped {} relationships with missing endpoints"),
    ("sources.failed", "同步失败", "Sync failed"),
    ("sources.cancelled", "已取消", "Cancelled"),
    ("sources.read_only", "图以只读方式打开，不能同步", "The graph is open read-only; it can't be synced"),
    ("sources.last_ok", "{}同步，{} 个节点，{} 条关系", "Synced {}, {} nodes, {} relationships"),
    ("sources.last_failed", "{}同步失败：{}", "Sync failed {}: {}"),
    ("sources.never", "从未同步", "Never synced"),
    ("sources.remove", "移除", "Remove"),
    ("sources.remove_title", "移除数据源 {}？", "Remove source {}?"),
    ("sources.remove_body", "只移除配置，已导入的数据会保留在图中。", "Only the configuration is removed; imported data stays in the graph."),

    // ── plugins ─────────────────────────────────────────────────────────
    ("plugins.builtin", "内置", "Built-in"),
    ("plugins.install", "安装插件…", "Install Plugin…"),
    ("plugins.installed", "已安装 {} {}", "Installed {} {}"),
    ("permissions.title", "允许这个插件运行？", "Allow this plugin to run?"),
    ("permissions.body", "插件在本机运行，会获得下面声明的权限。只安装你信任的插件。", "The plugin runs on this computer with the permissions below. Only install plugins you trust."),
    ("permissions.approve", "允许", "Allow"),
    ("permissions.declared", "声明的权限", "Declared permissions"),
    ("permissions.network", "访问网络：{}", "Network access: {}"),
    ("permissions.read_files", "读取你选择的文件", "Read files you choose"),
    ("permissions.commands", "运行命令：{}", "Run commands: {}"),
    ("permissions.none", "不需要额外权限", "No extra permissions"),
    // ── carried over from DuckLocal: dashboards, charts, maps, comments ──
    ("chart.empty.detected_columns", "检测到的列：{}", "Detected columns: {}"),
    ("chart.empty.detected_more", "{} … 等 {} 列", "{} … and {} more columns"),
    ("chart.empty.hint", "提示：在查询里返回数值，例如 count(n) 或数值属性。", "Tip: return a number from the query, such as count(n) or a numeric property."),
    ("chart.empty.no_numeric", "当前结果没有数值列，无法绘制图表。", "This result has no numeric columns to chart."),
    ("chart.empty.no_rows", "没有可绘制的数据行。", "No data rows to plot."),
    ("chart.map.colored_by", "按 {} 着色", "Colored by {}"),
    ("chart.map.no_label", "（结果中没有名称列）", "(no name column in the result)"),
    ("chart.map.notice.capped", "仅绘制前 {} / {} 个点", "Plotting the first {} of {} points"),
    ("chart.map.notice.dropped", "{} 行缺少有效经纬度，未绘制", "{} rows without valid coordinates not plotted"),
    ("chart.map.notice.outside", "{} 个远离其他点的点在视图之外", "{} points far from the rest lie outside the view"),
    ("chart.map.notice.unsized", "{} 个点没有可用的大小值，按最小绘制", "{} points have no usable size value; drawn smallest"),
    ("chart.map.other", "其他", "Other"),
    ("chart.map.row", "第 {} 行", "Row {}"),
    ("chart.map.sized_by", "按 {} 定大小", "Sized by {}"),
    ("chart.map.title", "地图（{} / {}）", "Map of {} / {}"),
    ("chart.notice.bars_capped", "仅显示前 {} / {} 行", "Showing the first {} of {} rows"),
    ("chart.notice.downsampled", "已降采样：{} 点 → {} 点（每点为 {} 个采样的均值）", "Downsampled: {} points → {} points (each point is the average of {} samples)"),
    ("chart.notice.series_capped", "仅显示前 {} / {} 个数值列", "Showing the first {} of {} numeric columns"),
    ("chart.pie.no_positive", "饼图需要正数值，当前第一个数值列没有正数。", "A pie needs positive values; the first numeric column has none."),
    ("chart.pie.notice.skipped", "{} 行为零、负数或空值，未计入", "{} rows with zero, negative or missing values left out"),
    ("chart.pie.title", "{} 占比（按 {}）", "Share of {} by {}"),
    ("chart.pie.total", "合计 {}", "{} total"),
    ("chart.title.by", "{}（按 {}）", "{} by {}"),
    ("chart.title.point_count", "绘制 {} 点", "{} points plotted"),
    ("chart.title.series_more", "{} … 共 {} 列", "{} … {} columns total"),
    ("dashboard.card.first_row", "显示的是 {} 行中的第一行", "Showing the first of {} rows"),
    ("dashboard.card.no_rows", "查询没有返回行", "The query returned no rows"),
    ("dashboard.comments.agent", "Agent", "Agent"),
    ("dashboard.comments.cancel", "取消", "Cancel"),
    ("dashboard.comments.placeholder", "写下要改的地方…", "What should change?"),
    ("dashboard.comments.reopen", "重新打开", "Reopen"),
    ("dashboard.comments.reply", "回复", "Reply"),
    ("dashboard.comments.replying", "回复 {}", "Replying to {}"),
    ("dashboard.comments.resolve", "解决", "Resolve"),
    ("dashboard.comments.resolved", "已解决", "Resolved"),
    ("dashboard.comments.send", "评论", "Comment"),
    ("dashboard.comments.shortcut", "Ctrl/⌘ + Enter 发送", "Ctrl/⌘ + Enter to send"),
    ("dashboard.comments.show_tooltip", "评论这张图，交给 Agent 审阅修改", "Comment on this plot for an agent to review"),
    ("dashboard.comments.you", "你", "You"),
    ("dashboard.conflict", "文件在磁盘上已被修改", "The file changed on disk"),
    ("dashboard.conflict.hint", "你的修改尚未保存；保存会覆盖磁盘上的改动。", "You have unsaved edits; saving overwrites the changes on disk."),
    ("dashboard.empty", "这个 spec 没有声明任何 plot 块。", "This spec declares no plot blocks."),
    ("dashboard.filter.clear", "清除此筛选", "Clear this filter"),
    ("dashboard.filter.clear_all", "全部清除", "Clear all"),
    ("dashboard.filter.hint", "点击可筛选其他图表", "Click to filter the other plots"),
    ("dashboard.filter.label", "筛选", "Filtered by"),
    ("dashboard.invalid_plot", "这个 plot 没有声明 y 列，无法绘制。", "This plot declares no y column to draw."),
    ("dashboard.map_no_coordinates", "地图需要经纬度列：请用 lat 和 lng 指定。查询返回的列：{}", "A map needs coordinate columns: name them with lat and lng. The query returns: {}"),
    ("dashboard.missing_column_named", "查询结果中没有列 {}。可用的列：{}", "The query result has no column {}. Available columns: {}"),
    ("dashboard.not_updated", "Dashboard 未更新", "Dashboard not updated"),
    ("dashboard.notice.scatter_thinned", "显示 {} 个点（共 {} 个，沿 x 轴均匀抽样）。", "Showing {} of {} points, thinned evenly along x."),
    ("dashboard.notice.zero_filled", "{} 个（系列 × x）组合没有数据，按 0 补齐", "{} (series, x) cells had no data and were filled with 0"),
    ("dashboard.picker.not_a_spec", "{} 不是 .dash 文件", "{} is not a .dash file"),
    ("dashboard.picker.prompt", "选择 .dash 文件", "Choose a .dash file"),
    ("dashboard.plot_failed", "这个图没有画出来", "This plot could not be drawn"),
    ("dashboard.query_missing", "plot 引用的查询 {} 不存在", "The query {} a plot references does not exist"),
    ("dashboard.reload", "重新加载", "Reload"),
    ("dashboard.reload.tooltip", "重新读取 spec 文件，并重跑它的所有查询", "Re-read the spec file and re-run its queries"),
    ("dashboard.restore.gone", "无法恢复上次打开的仪表盘 {}（{}）：文件已不存在", "Could not restore dashboard {} ({}) from last session: the file is gone"),
    ("dashboard.save", "保存", "Save"),
    ("dashboard.save.tooltip", "把修改写回 .dash 文件并重跑", "Write changes back to the .dash file and re-run"),
    ("dashboard.save_failed", "保存 {} 失败：{}", "Could not save {}: {}"),
    ("dashboard.source.back", "返回 dashboard", "Back to dashboard"),
    ("dashboard.source.show", "查看 spec", "View spec"),
    ("dashboard.source.tooltip", "阅读这个 dashboard 的 .dash 源码", "Read this dashboard's .dash source"),
    ("dashboard.source.unreadable", "无法读取 {}：{}", "Could not read {}: {}"),
    ("dashboard.spec_failed", "仪表盘 spec 无法加载", "The dashboard spec could not be loaded"),
    ("dashboard.sql.copy", "复制", "Copy"),
    ("dashboard.sql.hide", "图表", "Chart"),
    ("dashboard.sql.hide_tooltip", "回到图表", "Back to the plot"),
    ("dashboard.sql.open", "在查询页打开", "Open in a query tab"),
    ("dashboard.sql.show_tooltip", "查看这张图的查询", "Show this plot's query"),
    ("presence.adding", "{} 正在加 “{}”…", "{} is adding “{}”…"),
    ("presence.dismiss", "知道了", "Dismiss"),
    ("presence.done", "已完成", "Done"),
    ("presence.editing", "{} 正在修改", "{} is editing"),
    ("presence.has_errors", "有错误", "Has errors"),
    ("presence.list_separator", "、", ", "),
    ("presence.log.checked", "检查了一遍", "Ran a check"),
    ("presence.log.read", "看了 {}", "Read {}"),
    ("presence.log.replied", "回复了 {} 上的评论", "Replied on {}"),
    ("presence.removed", "{} 删除了 {}", "{} removed {}"),
    ("presence.session", "{} · 这次工作", "{} · this session"),
    ("presence.waiting", "{} 条评论在等你回复", "{} thread(s) waiting on you"),
    ("presence.working", "工作中", "Working"),
    ("results.running", "正在运行查询…", "Running query…"),
    ("workspace.open_dashboard", "打开 Dashboard", "Open dashboard"),

    // ── dashboards (KhonsuGraph) ──────────────────────────────────────────
    ("dashboard.graph.counts", "{} 个节点，{} 条关系", "{} nodes, {} relationships"),
    ("dashboard.needs_graph", "先打开一个图库：仪表盘查询的是当前打开的图库。", "Open a graph first: a dashboard queries the open graph."),
    ("dashboard.query_writes", "查询 {} 会写入（{}）；仪表盘只读取图库", "query {} writes ({}); a dashboard only reads the graph"),
    ("dashboard.restore.gone_path", "上次打开的仪表盘已不存在：{}", "A dashboard open last time is gone: {}"),
];

#[cfg(test)]
mod tests {
    use super::{format_template, join_in, translate, Language};

    #[test]
    fn a_sentence_follows_a_full_stop_not_a_comma() {
        assert_eq!(
            join_in(Language::En, "trips by payment", "Click to filter"),
            "trips by payment. Click to filter"
        );
        assert_eq!(join_in(Language::Zh, "按标签", "点击可筛选"), "按标签，点击可筛选");
    }

    #[test]
    fn translate_falls_back_to_zh_then_key() {
        assert_eq!(translate(Language::En, "sidebar.tab.schema"), "Schema");
        assert_eq!(translate(Language::Zh, "sidebar.tab.schema"), "结构");
        assert_eq!(translate(Language::En, "no.such.key"), "no.such.key");
    }

    #[test]
    fn every_key_has_both_locales_and_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for (key, zh, en) in super::STRINGS {
            assert!(seen.insert(key), "duplicate key: {key}");
            assert!(!zh.is_empty(), "empty zh for {key}");
            assert!(!en.is_empty(), "empty en for {key}");
        }
    }

    #[test]
    fn format_template_substitutes_sequentially() {
        assert_eq!(format_template("{} rows · {} cols", &["3", "2"]), "3 rows · 2 cols");
        assert_eq!(format_template("{} {}", &["a"]), "a {}");
    }
}
