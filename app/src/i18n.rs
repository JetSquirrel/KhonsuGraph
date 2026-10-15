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
