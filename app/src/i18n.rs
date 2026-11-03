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
    ("menu.check_updates", "检查更新…", "Check for Updates…"),
    ("menu.check_updates_at_launch", "启动时检查更新", "Check for Updates at Launch"),
    ("status_bar.update.check", "检查更新", "Check for updates"),
    ("status_bar.update.checking", "正在检查更新…", "Checking for updates…"),
    ("status_bar.update.latest", "已是最新", "Up to date"),
    ("status_bar.update.available", "有新版本 {}", "Update available: {}"),
    ("status_bar.update.downloading", "正在下载 {}…", "Downloading {}…"),
    ("status_bar.update.restart", "重启以更新到 {}", "Restart to update to {}"),
    ("status_bar.update.restart_hint", "新版本已下载，重启或下次退出时安装", "Downloaded; installs when KhonsuGraph restarts or next quits"),
    ("status_bar.update.failed", "检查更新失败", "Update check failed"),
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
    ("menu.base_map", "在线底图（OpenStreetMap）", "Online base map (OpenStreetMap)"),
    ("crash.title", "KhonsuGraph 上次意外退出", "KhonsuGraph quit unexpectedly last time"),
    ("crash.notice", "原因：{}。报告已保存在日志文件夹中，反馈问题时可以附上。", "Cause: {}. A report was saved in the logs folder; attach it when you report the problem."),
    ("crash.show_report", "显示报告", "Show report"),

    // ── opening graphs ──────────────────────────────────────────────────
    ("notify.open.failed_title", "无法打开图", "Couldn't open the graph"),
    ("notify.open.locked_title", "图正被占用", "The graph is in use"),
    ("notify.open.failed", "打开失败：{}", "Opening failed: {}"),
    ("notify.open.locked", "另一个进程正在写这个图：{}", "Another process is writing to this graph: {}"),
    ("notify.open.unreadable", "这个文件夹看起来是图，但无法读取，可能已损坏。详情：{}", "This folder looks like a graph but can't be read. It may be damaged. Details: {}"),
    ("notify.open.not_a_graph", "{} 不是 HawDB 图目录", "{} is not a HawDB graph directory"),
    ("notify.open.not_empty", "{} 不是空目录，不能在这里新建图", "{} is not empty; a new graph needs an empty folder"),
    ("error.no_graph", "还没有打开图", "No graph is open"),

    // ── welcome ─────────────────────────────────────────────────────────
    ("home.title", "首页", "Home"),
    ("home.purpose", "打开或导入关联数据，在图上查看对象与关系。", "Open or import connected data, and see things and how they relate on a graph."),
    ("home.import_csv", "导入 CSV…", "Import CSV…"),
    ("home.continue", "继续上次工作", "Continue where you left off"),
    ("home.counts", "{} 个节点，{} 条关系", "{} nodes, {} relationships"),
    ("home.labels", "节点类型", "Node types"),
    ("home.more_labels", "还有 {} 种", "and {} more"),
    ("home.browse_nodes", "浏览节点", "Browse nodes"),
    ("home.view_graph", "查看图", "View graph"),
    ("home.write_query", "编写查询", "Write a query"),
    ("home.add_data", "添加数据…", "Add data…"),
    ("home.sample", "浏览和查看图只显示前面的一部分数据，不是全部。", "Browsing and viewing show only the first part of the data, not all of it."),
    ("home.empty", "这个图还没有数据。可以导入 CSV，或连接其他数据源。", "This graph has no data yet. Import a CSV, or connect another data source."),
    ("home.empty_read_only", "这个图还没有数据。它以只读方式打开，所以不能在这里添加。", "This graph has no data yet. It's open read-only, so data can't be added here."),
    ("home.nodes_only", "这个图只有节点，还没有关系。", "This graph has nodes but no relationships yet."),
    ("graph_dialog.browse", "浏览…", "Browse…"),
    ("graph_dialog.open.explain", "图保存在一个文件夹中。请选择之前创建的图文件夹；如果要用 CSV 文件，请使用导入。", "A graph is saved as a folder. Choose a graph folder you created before. For a CSV file, use Import instead."),
    ("graph_dialog.open.placeholder", "图文件夹的路径", "Path to the graph folder"),
    ("graph_dialog.open.submit", "打开", "Open"),
    ("graph_dialog.check.graph", "可以打开：{}", "Ready to open: {}"),
    ("graph_dialog.check.missing", "找不到这个路径。", "This path doesn't exist."),
    ("graph_dialog.check.csv", "这是一个数据文件，不是图。要使用它，请选择导入 CSV。", "This is a data file, not a graph. To use it, choose Import CSV."),
    ("graph_dialog.check.file", "这是一个文件。图是文件夹。", "This is a file. A graph is a folder."),
    ("graph_dialog.check.inside", "这是图内部的文件夹。请选择图本身：{}", "This folder is inside a graph. Choose the graph itself: {}"),
    ("graph_dialog.check.holds", "这个文件夹里有图，但它本身不是图。请选择其中一个：{}", "This folder holds graphs but isn't one. Choose one of them: {}"),
    ("graph_dialog.check.not_a_graph", "这个文件夹不是图。如果要在这里放新图，请使用新建图。", "This folder isn't a graph. To put a new one here, use New Graph."),
    ("graph_dialog.new.explain", "应用会在你选择的位置创建一个以图命名的文件夹。", "A folder with the graph's name is created in the place you choose."),
    ("graph_dialog.import.explain", "导入的数据保存在图中，所以先给新图起个名字。下一步选择 CSV 文件。", "Imported data is kept in a graph, so name a new one first. You'll choose the CSV file next."),
    ("graph_dialog.import.submit", "下一步", "Next"),
    ("graph_dialog.import.into_open", "导入到 {}", "Import into {}"),
    ("graph_dialog.new.submit", "创建", "Create"),
    ("graph_dialog.new.name", "名称", "Name"),
    ("graph_dialog.new.name_placeholder", "例如：服务依赖", "For example: service-map"),
    ("graph_dialog.new.location", "存放位置", "Location"),
    ("graph_dialog.new.location_placeholder", "放置图文件夹的位置", "Where to put the graph's folder"),
    ("graph_dialog.new.probing", "正在寻找可以创建图的位置…", "Looking for a place a graph can be created…"),
    ("graph_dialog.new.no_suggestion", "没有找到可以创建图的默认位置，请选择一个。在 Windows 上，C 盘可能无法创建图，可以试试其他磁盘。", "No default place could hold a graph, so please choose one. On Windows, graphs may not work on drive C, so try another drive."),
    ("graph_dialog.new.will_create", "将创建：{}", "Will create: {}"),
    ("graph_dialog.new.no_name", "请输入名称。", "Enter a name."),
    ("graph_dialog.new.bad_name", "这个名称不能用作文件夹名。", "This name can't be used for a folder."),
    ("graph_dialog.new.no_location", "请选择存放位置。", "Choose a location."),
    ("graph_dialog.new.location_missing", "找不到这个位置。", "This location doesn't exist."),
    ("graph_dialog.new.taken", "{} 已存在且不为空。请换个名称或位置。", "{} already exists and isn't empty. Choose another name or location."),
    ("welcome.recent", "最近打开", "Recent"),
    ("welcome.no_recent", "还没有打开过图", "No recent graphs"),
    ("welcome.read_only", "只读打开", "Open read-only"),
    ("welcome.forget", "从列表移除", "Remove from list"),
    ("welcome.privacy", "数据只保存在本机，不会上传。", "Your data stays on this computer and is never uploaded."),

    // ── workspace ───────────────────────────────────────────────────────
    ("workspace.run", "运行", "Run"),
    ("workspace.run.tooltip", "运行查询", "Run the query"),
    ("workspace.explain", "执行计划", "Explain"),
    ("workspace.explain.tooltip", "查看 HawDB 为这条查询选择的执行计划", "Show the plan HawDB chooses for this query"),
    ("workspace.selection_hint", "选中部分文本时只运行选中内容", "With text selected, only the selection runs"),
    ("workspace.empty_hint", "输入查询，或从左侧结构与历史中选择", "Enter a query, or choose one from Schema or History in the sidebar"),
    ("workspace.add_tab", "新建查询", "New query"),
    ("workspace.new_cypher", "新建 Cypher 查询", "New Cypher Query"),
    ("workspace.new_sql", "新建 SQL 查询", "New SQL Query"),
    ("workspace.tab.default_title", "查询 {}", "Query {}"),

    // ── results ─────────────────────────────────────────────────────────
    ("results.table", "表格", "Table"),
    ("results.graph", "图", "Graph"),
    ("results.plan", "执行计划", "Plan"),
    ("results.rows", "{} 行", "{} rows"),
    ("results.rows_truncated", "显示 {} / {} 行", "{} of {} rows"),
    ("results.truncated", "只显示前 {} 行", "Showing the first {} rows"),
    ("results.empty", "运行查询后，结果显示在这里", "Run a query to see results here"),
    ("results.no_rows", "查询已完成，没有返回数据行", "Query completed with no rows returned"),
    ("results.no_rows_hint", "读取查询可尝试放宽条件；写入语句可能不返回数据行。", "For a read query, try broader conditions. Write statements may return no rows."),
    ("results.no_matches", "没有匹配的结果", "No matching results"),
    ("results.no_matches_hint", "试试其他关键词，或清除筛选查看已有结果。", "Try another search term, or clear the filter to see the loaded results."),
    ("results.filtered_rows", "匹配 {} / {} 行", "{} of {} loaded rows match"),
    ("results.clear_filter", "清除筛选", "Clear filter"),
    ("results.copy_error", "复制错误", "Copy error"),
    ("results.error_at", "第 {} 行，第 {} 列", "Line {}, column {}"),
    ("results.error_line", "第 {} 行", "Line {}"),
    ("results.export_csv_hint", "导出 CSV：已加载的全部行，不受结果筛选影响", "Export CSV: every loaded row, regardless of the result filter"),
    ("results.exported", "已导出到 {}", "Exported to {}"),
    ("results.filter", "筛选结果…", "Filter results…"),
    ("results.copy_row", "复制此行", "Copy row"),
    ("results.copied", "已复制", "Copied"),

    // ── command line & AI (after DuckLocal) ─────────────────────────────
    ("setup.title", "命令行与 AI", "Command line & AI"),
    ("setup.command.heading", "终端命令", "Terminal command"),
    ("setup.command.description", "安装后可在任意终端运行 khonsu，查询图、同步数据源、检查看板，结果为 JSON。", "Run khonsu in any terminal to query graphs, sync data sources and check dashboards, with JSON back."),
    ("setup.command.installed", "已安装在 {}", "Installed at {}"),
    ("setup.command.elsewhere", "{} 指向另一个 KhonsuGraph；重新安装会改为指向这一个。", "{} points to another copy of KhonsuGraph; installing again points it here."),
    ("setup.command.not_installed", "尚未安装", "Not installed"),
    ("setup.command.install", "安装命令", "Install command"),
    ("setup.command.reinstall", "重新安装", "Reinstall"),
    ("setup.ai.heading", "AI 助手", "AI agents"),
    ("setup.ai.description", "让 AI 助手用 KhonsuGraph 探索你的图：先读结构，再写 Cypher 或 SQL，编写看板并回复你在看板上的评论。", "Let an AI agent explore your graphs with KhonsuGraph: it reads the schema, writes Cypher or SQL, authors dashboards and answers your comments on them."),
    ("setup.ai.claude", "Claude Code", "Claude Code"),
    ("setup.ai.claude.installed", "已添加 KhonsuGraph 技能", "KhonsuGraph skill added"),
    ("setup.ai.claude.outdated", "技能有新版本", "A newer skill is available"),
    ("setup.ai.claude.missing", "尚未添加技能", "Skill not added"),
    ("setup.ai.claude.install", "添加到 Claude Code", "Add to Claude Code"),
    ("setup.ai.claude.update", "更新技能", "Update skill"),
    ("setup.ai.other", "其他 AI 助手", "Other agents"),
    ("setup.ai.other.description", "复制一段说明，粘贴给 Codex、Cursor 或任何 AI 助手。", "Copy a brief to paste into Codex, Cursor or any other agent."),
    ("setup.ai.other.copy", "复制说明", "Copy brief"),
    ("setup.error.move_to_applications", "请先把 KhonsuGraph 拖到“应用程序”文件夹，再安装命令。", "Move KhonsuGraph to the Applications folder first, then install the command."),
    ("setup.error.not_ours", "{} 已存在，且不是 KhonsuGraph 创建的，未作改动。", "{} already exists and was not made by KhonsuGraph, so it was left alone."),
    ("setup.error.cancelled", "已取消安装。", "Installation cancelled."),
    ("notify.setup.command_installed", "已安装 khonsu 命令，打开新的终端即可使用。", "Installed the khonsu command. Open a new terminal to use it."),
    ("notify.setup.command_needs_path", "已安装在 {}，但该目录不在 PATH 中；把它加入 PATH 后即可使用。", "Installed at {}, but that folder is not on your PATH; add it to use the command."),
    ("notify.setup.command_failed", "无法安装命令：{}", "Could not install the command: {}"),
    ("notify.setup.skill_installed", "已添加到 Claude Code，新会话即可使用：{}", "Added to Claude Code; new sessions can use it: {}"),
    ("notify.setup.skill_failed", "无法添加技能：{}", "Could not add the skill: {}"),
    ("notify.setup.prompt_copied", "已复制，粘贴给你的 AI 助手即可。", "Copied. Paste it into your agent."),
    ("rebuild.title", "这个图需要重建", "This graph needs rebuilding"),
    ("rebuild.body", "{} 由较早的 HawDB（0.8–0.15）写成，当前版本无法读取，也没有迁移方式。可以从它的数据源重建：{}。旧文件会原样保留在旁边（.hawdb-old）；手工写入、不在数据源里的内容不会带过来。", "{} was written by an older HawDB (0.8–0.15), which this version cannot read or migrate. It can be rebuilt from its data sources: {}. The old files stay beside it untouched (.hawdb-old); anything written by hand rather than synced is not carried over."),
    ("rebuild.missing_plugins", "{} 由较早的 HawDB（0.8–0.15）写成，可以从数据源重建，但它需要的插件不可用：{}。重建只能进行一次，缺少插件的数据源会丢失数据。", "{} was written by an older HawDB (0.8–0.15) and can be rebuilt from its data sources, but the plugin it needs is not available: {}. A graph is rebuilt only once, and a source without its plugin would leave its data out."),
    ("rebuild.no_sources", "{} 由较早的 HawDB（0.8–0.15）写成，当前版本无法读取。它没有配置数据源，无法自动重建。", "{} was written by an older HawDB (0.8–0.15), which this version cannot read. It has no data sources, so it cannot be rebuilt automatically."),
    ("rebuild.action", "从数据源重建", "Rebuild from data sources"),
    ("rebuild.running", "正在重建 {}…", "Rebuilding {}…"),
    ("rebuild.done", "已重建：{} 个节点，{} 条关系。旧文件在 {}", "Rebuilt: {} nodes, {} relationships. The old files are in {}"),
    ("rebuild.failed", "重建失败", "Rebuild failed"),
    // ── graph canvas ────────────────────────────────────────────────────
    ("canvas.fit", "适配", "Fit"),
    ("canvas.relayout", "重新布局", "Re-layout"),
    ("canvas.maximize", "铺满窗口", "Fill the window"),
    ("canvas.restore", "退出铺满（Esc）", "Back in place (Esc)"),
    ("canvas.maximized_note", "图已铺满窗口，按 Esc 返回", "The graph fills the window; press Esc to return"),
    ("canvas.group_by", "分组：{}", "Group: {}"),
    ("canvas.group", "分组", "Group"),
    ("canvas.group_none", "不分组", "No groups"),
    ("canvas.collapse_groups", "全部折叠", "Collapse all"),
    ("canvas.expand_groups", "全部展开", "Expand all"),
    ("canvas.expand", "展开邻居", "Expand neighbors"),
    ("canvas.hint", "双击节点展开邻居，右侧查看属性", "Double-click a node to expand it; select one to see its properties"),
    ("canvas.compact_hint", "滚轮缩放，悬停查看名称", "Scroll to zoom; hover to see names"),
    ("canvas.truncated", "显示 {} / {} 个节点", "Showing {} of {} nodes"),
    ("canvas.limit", "画布最多显示 {} 个节点，没有展开", "The canvas holds at most {} nodes; not expanded"),
    ("evidence.title", "证据", "Evidence"),
    ("evidence.id", "证据 ID", "Evidence ID"),
    ("evidence.source", "数据源", "Source"),
    ("evidence.plugin", "插件", "Plugin"),
    ("evidence.written", "记录最后写入", "Record last written"),
    ("evidence.last_sync", "数据源最后成功同步", "Source last synced"),
    ("evidence.snapshot", "数据源快照", "Source snapshot"),
    ("evidence.coverage", "最近一次同步覆盖范围", "Last sync's coverage"),
    ("evidence.coverage.complete", "完整", "Complete"),
    ("evidence.coverage.partial", "部分", "Partial"),
    ("evidence.coverage.failed", "失败", "Failed"),
    ("evidence.coverage.cancelled", "已取消", "Cancelled"),
    ("evidence.assertion", "断言", "Assertion"),
    ("evidence.explicit", "数据源报告", "Reported by the source"),
    ("evidence.derived", "由其他记录推导", "Derived from other records"),
    ("evidence.unknown", "未提供", "Not supplied"),
    ("evidence.not_stable", "内部 ID，重建图后可能改变", "An internal ID, which a rebuild can change"),
    ("evidence.redacted", "已遮盖疑似机密：{}", "Masked as possible secrets: {}"),
    ("evidence.copy", "复制证据 JSON", "Copy evidence as JSON"),
    ("evidence.copy_path", "复制路径证据 JSON", "Copy path evidence as JSON"),
    ("investigation.open", "打开调查…", "Open investigation…"),
    ("investigation.save", "保存调查…", "Save investigation…"),
    ("investigation.saved", "已保存调查：{}", "Investigation saved: {}"),
    ("investigation.save_failed", "无法保存调查：{}", "The investigation could not be saved: {}"),
    ("investigation.open_failed", "无法打开调查：{}", "The investigation could not be opened: {}"),
    ("investigation.graph_missing", "这个调查的图不在 {}", "This investigation's graph is not at {}"),
    ("investigation.reopened", "已重新打开调查“{}”，保存于 {}", "Reopened investigation \"{}\", saved {}"),
    ("investigation.changed", "图在保存后发生了变化，结果可能与保存时不同", "The graph has changed since this was saved, so the answer may differ"),
    ("investigation.not_compared", "图太大，无法比较内容，只确认了记录数相同", "The graph is too large to compare, so only its record counts were checked"),
    ("investigation.missing_seeds", "保存的起点已不在图中：{}", "Saved starting points are no longer in the graph: {}"),
    ("investigation.target_missing", "保存的路径终点已不在图中", "The saved path's end is no longer in the graph"),
    ("investigation.path_differs", "路径不再基于保存时的记录", "The path no longer rests on the saved records"),
    ("investigation.unsure", "这些记录靠内部 ID 找到，图已变化，可能已是别的记录：{}", "Found by internal ID on a changed graph, so these may now be other records: {}"),
    ("analysis.check", "运行分析", "Run analysis"),
    ("analysis.recheck", "重新检查", "Check again"),
    ("analysis.stop", "停止", "Stop"),
    ("analysis.failed", "检查失败：{}", "The check failed: {}"),
    ("analysis.incomplete", "图太大未能读完，或有数据源最近一次同步不完整，所以缺少发现不说明任何问题。", "The graph was too large to read whole, or a source's last sync was not complete, so a missing finding means nothing."),
    ("analysis.none", "没有关于此节点的发现。如果刚同步过，请重新检查。", "No findings about this one. If you just synced, check again."),
    ("analysis.status.met", "满足", "Met"),
    ("analysis.status.not_met", "不满足", "Not met"),
    ("analysis.status.unknown", "未知", "Unknown"),
    ("analysis.assumptions", "未检查", "Not checked"),
    ("analysis.show_path", "在画布上显示路径", "Show path on canvas"),
    ("analysis.copy", "复制发现 JSON", "Copy finding as JSON"),
    ("explore.title", "路径探索", "Path Explorer"),
    ("explore.out", "沿箭头", "Outbound"),
    ("explore.in", "逆箭头", "Inbound"),
    ("explore.both", "双向", "Both"),
    ("explore.hops", "最多 {} 跳", "Up to {} hops"),
    ("explore.fewer_hops", "减少跳数", "Fewer hops"),
    ("explore.more_hops", "增加跳数", "More hops"),
    ("explore.types_all", "全部关系类型", "All relationship types"),
    ("explore.types_some", "关系类型：{} 种", "Relationship types: {}"),
    ("explore.add_start", "加为起点", "Add as a start"),
    ("explore.remove_start", "移出起点", "Remove from starts"),
    ("explore.starts", "起点：{} 个", "Starts: {}"),
    ("explore.run", "探索", "Explore"),
    ("explore.cancel", "取消", "Cancel"),
    ("explore.back", "返回上一步", "Back"),
    ("explore.show_path", "只看这条路径", "Show only this path"),
    ("explore.path", "到此节点的路径", "Path to this node"),
    ("explore.found", "节点：{}，关系：{}，深度：{}", "Nodes: {}, relationships: {}, depth: {}"),
    ("explore.none", "没有符合筛选条件的关系", "No relationships match these filters"),
    ("explore.limit.hops", "已到跳数上限，可能还有更远的节点", "Stopped at the hop limit; there is more beyond it"),
    ("explore.limit.nodes", "已到节点上限，结果不完整", "Stopped at the node limit; this result is incomplete"),
    ("explore.limit.edges", "已到关系上限，结果不完整", "Stopped at the relationship limit; this result is incomplete"),
    ("explore.cancelled", "已取消，结果不完整", "Cancelled; this result is incomplete"),
    ("explore.missing_start", "有起点已不在图中", "A start is no longer in the graph"),
    ("explore.failed", "探索失败：{}", "Exploring failed: {}"),
    ("explore.candidate", "路径来自配置数据，不代表实际可达", "A path in the data, not proof that it can be reached"),

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
    ("sources.edit", "编辑数据源", "Edit source"),
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
    ("sources.last_partial", "{} 同步不完整，未删除任何记录：{}", "{} incomplete, nothing removed: {}"),
    ("sources.never", "从未同步", "Never synced"),
    ("sources.remove", "移除", "Remove"),
    ("sources.remove_title", "移除数据源 {}？", "Remove source {}?"),
    ("sources.remove_body", "只移除配置，已导入的数据会保留在图中。", "Only the configuration is removed; imported data stays in the graph."),

    // ── plugins ─────────────────────────────────────────────────────────
    ("plugins.builtin", "内置", "Built-in"),
    ("plugins.install", "安装插件…", "Install Plugin…"),
    ("plugins.installed", "已安装 {} {}", "Installed {} {}"),
    ("permissions.title", "允许这个插件运行？", "Allow this plugin to run?"),
    ("permissions.body", "插件不在沙箱中运行：它以你的身份运行，能访问你能访问的一切。下面列出的是它自己声明的内容，KhonsuGraph 不会强制执行。只允许你信任的插件。", "This plugin is not sandboxed. It runs as you and can reach whatever you can. The list below is what it says it does, and KhonsuGraph does not enforce it. Only allow plugins you trust."),
    ("permissions.changed", "这个插件在获得允许之后发生了变化。请确认你信任现在的版本。", "This plugin has changed since you allowed it. Check that you trust it as it is now."),
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
    ("dashboard.reload.tooltip", "重新读取 spec 文件，并重跑它的所有查询", "Re-read the spec file and re-run its queries"),
    ("dashboard.restore.gone", "无法恢复上次打开的仪表盘 {}（{}）：文件已不存在", "Could not restore dashboard {} ({}) from last session: the file is gone"),
    ("dashboard.save.tooltip", "把修改写回 .dash 文件并重跑", "Write changes back to the .dash file and re-run"),
    ("dashboard.save_failed", "保存 {} 失败：{}", "Could not save {}: {}"),
    ("dashboard.source.back", "返回视图", "Back to dashboard"),
    ("dashboard.source.tooltip", "阅读这个 dashboard 的 .dash 源码", "Read this dashboard's .dash source"),
    ("dashboard.source.unreadable", "无法读取 {}：{}", "Could not read {}: {}"),
    ("dashboard.spec_failed", "仪表盘 spec 无法加载", "The dashboard spec could not be loaded"),
    ("dashboard.sql.copy", "复制查询", "Copy the query"),
    ("dashboard.sql.hide_tooltip", "回到图表", "Back to the plot"),
    ("dashboard.maximize.tooltip", "放大面板", "Enlarge panel"),
    ("dashboard.restore_plot.tooltip", "放回看板（Esc）", "Back to the dashboard (Esc)"),
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
    fn placeholders_are_plain_and_match_across_locales() {
        // `format_template` fills only `{}`; a `{0}` would show as is.
        for (key, zh, en) in super::STRINGS {
            for text in [zh, en] {
                let braces = text.matches('{').count();
                assert_eq!(braces, text.matches("{}").count(), "only {{}} placeholders in {key}: {text}");
            }
            assert_eq!(zh.matches("{}").count(), en.matches("{}").count(), "placeholder count differs in {key}");
        }
    }

    #[test]
    fn format_template_substitutes_sequentially() {
        assert_eq!(format_template("{} rows · {} cols", &["3", "2"]), "3 rows · 2 cols");
        assert_eq!(format_template("{} {}", &["a"]), "a {}");
    }
}
