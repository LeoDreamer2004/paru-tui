use super::settings::Language;

// UI translations are kept separate from backend messages and package metadata.
pub fn translate(text: &str, language: Language) -> String {
    if language == Language::En {
        return text.to_owned();
    }
    if text.contains('\n') {
        return text
            .split('\n')
            .map(|line| translate(line, language))
            .collect::<Vec<_>>()
            .join("\n");
    }
    for (prefix, translated) in [
        ("Search failed: ", "搜索失败："),
        (
            "AUR search failed; repository results are still available: ",
            "AUR 搜索失败，仍可查看官方结果：",
        ),
        ("AUR details failed: ", "AUR 详情读取失败："),
        ("AUR lookup failed: ", "AUR 来源识别失败："),
        ("AUR comments failed: ", "AUR 评论读取失败："),
        ("PKGBUILD view failed: ", "PKGBUILD 读取失败："),
        ("Cannot open AUR page: ", "无法打开 AUR 页面："),
        ("Cannot open comment link: ", "无法打开评论链接："),
        ("Error: ", "错误："),
        ("Transaction failed: ", "事务失败："),
        (
            "Post-transaction hooks failed; packages have already been changed: ",
            "事务后置钩子失败；软件包已变更，请检查并修复钩子错误：",
        ),
        ("Cannot start: ", "无法启动："),
        ("Proxy not saved: ", "代理未保存："),
        ("Cache directory: ", "缓存目录："),
        ("Database directory: ", "数据库目录："),
        ("Pacman cache cleanup failed: ", "Pacman 缓存清理失败："),
        ("AUR clone cleanup failed: ", "AUR 克隆清理失败："),
        ("Saved diff cleanup failed: ", "已保存差异清理失败："),
        ("pacman exited with code ", "pacman 退出码："),
    ] {
        if let Some(reason) = text.strip_prefix(prefix) {
            return format!("{translated}{}", translate(reason, language));
        }
    }
    static DYNAMIC: std::sync::OnceLock<Vec<(regex::Regex, &'static str)>> =
        std::sync::OnceLock::new();
    for (pattern, replacement) in DYNAMIC.get_or_init(|| {
        vec![
            (
                r"^(\d+) repository candidates  (\d+) AUR candidates$",
                "官方仓库可更新：$1  AUR 可更新：$2",
            ),
            (r"^Update (.+) to (.+)\?$", "将 $1 更新至 $2？"),
            (r"^Remove (.+)\?$", "删除 $1？"),
            (r"^Install (.+) (.+) from (.+)\?$", "从 $3 安装 $1 $2？"),
            (r"^Review changes for (.+):$", "审阅 $1 的修改："),
            (r"^Review build files for (.+):$", "审阅 $1 的构建文件："),
            (r"^Select provider for (.+):$", "为 $1 选择提供者："),
            (
                r"^Install ignored package (.+)\?$",
                "安装已忽略的软件包 $1？",
            ),
            (r"^Replace (.+) with (.+)\?$", "使用 $2 替换 $1？"),
            (r"^Remove corrupted file (.+)\?$", "删除损坏的文件 $1？"),
            (
                r"^Operation failed with exit code (\d+)$",
                "操作失败，退出码 $1",
            ),
            (
                r"^(\d+) cache entries failed: (.+)$",
                "$1 个缓存条目清理失败：$2",
            ),
            (
                r"^(.+): proxy (ON|OFF) · saved for future operations$",
                "$1：代理 $2，已保存",
            ),
            (
                r"^(?:Error: )?sudo: (\d+) incorrect password attempts(?::\s*)?$",
                "sudo 身份验证失败：密码错误 $1 次",
            ),
            (r"^Error: (.+)$", "错误：$1"),
            (r"^Transaction failed: (.+)$", "事务失败：$1"),
            (r"^Cannot start: (.+)$", "无法启动：$1"),
        ]
        .into_iter()
        .map(|(pattern, replacement)| (regex::Regex::new(pattern).unwrap(), replacement))
        .collect()
    }) {
        if pattern.is_match(text) {
            return pattern.replace(text, *replacement).into_owned();
        }
    }
    let labels = [
        ("Use host:port or a complete proxy URL", "请输入主机:端口或完整代理地址"),
        ("Include a port, for example 127.0.0.1:7890", "请填写端口，例如 127.0.0.1:7890"),
        ("Configure a proxy URL before enabling proxy rules", "请先配置代理地址，再启用代理规则"),
        ("Invalid proxy URL", "代理地址无效"),
        ("Invalid paru-tui settings", "paru-tui 配置无效"),
        ("Use http(s)://host:port or socks5(h)://host:port", "请使用 http(s)://主机:端口 或 socks5(h)://主机:端口"),

        ("AUTHENTICATION", "身份验证"),
        ("SELECT GROUP PACKAGES", "选择包组成员"),
        ("Select group packages (empty = all):", "选择包组成员（留空表示全部）："),
        ("Cancelled by user", "用户已取消操作"),
        ("Transaction cancelled", "事务已取消"),
        ("Operation running · Ctrl+C interrupts it; q cannot abandon an update", "任务正在运行，按 Ctrl+C 中断后再退出"),
        ("REVIEW CHANGES", "审阅 PKGBUILD 修改"),
        ("REVIEW BUILD FILES", "审阅构建文件"),
        ("SELECT PROVIDER", "选择依赖提供者"),
        ("TRANSACTION PLAN", "确认事务计划"),
        ("SIGNING KEY", "确认签名密钥"),
        ("RESOLVE CONFLICT", "处理软件包冲突"),
        ("INPUT REQUIRED", "输入选项"),
        ("CONFIRM OPERATION", "确认操作"),
        ("Operation completed", "操作完成"),
        ("An operation is already running", "已有任务正在运行"),
        ("Accept these changes?", "接受这些修改？"),
        ("Accept these build files?", "接受这些构建文件？"),
        ("Update the selected source? The final transaction plan will be shown before installation.", "更新所选来源？安装前将显示最终事务计划。"),
        ("Only this AUR target and its required dependencies will be processed. Other AUR packages are not selected.", "仅处理此 AUR 包及其必要依赖，不更新其他 AUR 包。"),
        ("Dependencies and the final plan are checked by paru.", "paru 将检查依赖和最终事务计划。"),
        ("Remove conflicting package?", "移除冲突的软件包？"),
        ("Skip packages with unresolved dependencies?", "跳过依赖无法满足的软件包？"),
        ("Import signing key?", "导入签名密钥？"),
        ("Enter updates one AUR package; use u for repository updates", "Enter 更新单个 AUR 包；按 u 更新官方仓库"),
        ("Proxy rules are available for confirmed AUR package bases", "代理规则仅适用于已确认的 AUR 软件包"),
        ("Set a proxy URL first, then press p on an AUR package", "请先设置代理地址，再对 AUR 包按 p"),
        ("Enter saves  Esc discards  Ctrl+U clears", "Enter 保存  Esc 放弃  Ctrl+U 清空"),
        ("Enter confirm  Esc cancel  PgUp/PgDn read plan", "Enter 确认  Esc 取消  PgUp/PgDn 查看计划"),
        ("sudo: a password is required", "sudo 需要密码"),
        ("Sorry, try again.", "密码错误，请重试。"),

        ("Scanning packages", "正在扫描软件包"),
        ("Scan complete", "扫描完成"),
        ("Git update check failed:", "Git 更新检查失败："),
        ("Git update check timed out", "Git 更新检查超时"),
        ("latest-commit", "最新提交"),
        ("VERSION & SOURCE", "版本与来源"),
        ("INSTALLATION", "安装信息"),
        ("BUILD & LINKS", "构建与链接"),
        ("Version", "版本"),
        ("Source", "来源"),
        ("Package base", "软件包基名"),
        ("Available", "可更新版本"),
        ("Installed", "已安装版本"),
        ("Disk size", "磁盘大小"),
        ("Depends on", "依赖"),
        ("Network", "网络"),
        ("SYSTEM", "系统"),
        ("STATUS", "状态"),
        ("INSPECTOR", "详细信息"),
        ("INSTALLED", "已安装"),
        ("SEARCH PACKAGES", "搜索软件包"),
        ("SEARCH · AUR…", "搜索 · AUR…"),
        ("SEARCH RESULTS", "搜索结果"),
        ("Searching packages…", "正在搜索软件包…"),
        ("Build output appears here while a package is built.", "软件包构建时将在此显示输出。"),
        ("Press / to search by name or description", "按 / 输入名称或描述，再按 Enter 搜索"),
        ("AUR search timed out", "AUR 搜索超时"),
        ("AUR package no longer exists", "AUR 软件包已不存在"),
        ("AUR lookup timed out", "AUR 来源识别超时"),
        ("Not installed", "尚未安装"),
        ("Details not loaded", "详情尚未加载"),
        ("PACKAGE INFO", "软件包信息"),
        ("AUR METADATA", "AUR 信息"),
        ("Maintainer", "维护者"),
        ("Last modified", "最后修改"),
        ("First submitted", "首次提交"),
        ("Votes", "票数"),
        ("Popularity", "热度"),
        ("Date source", "日期来源"),
        ("Repository build date", "仓库构建日期"),
        ("AUR last modified", "AUR 最后修改日期"),
        ("Download size", "下载大小"),
        ("Build dependencies", "构建依赖"),
        ("Check dependencies", "检查依赖"),
        ("PACMAN / REPOSITORIES", "PACMAN / 官方仓库"),
        ("AUR / COMMUNITY", "AUR / 社区仓库"),
        ("SETTINGS", "设置"),
        ("CLEAN PACKAGE CACHE", "清理软件包缓存"),
        ("PKGBUILD is available for AUR packages only", "只有 AUR 软件包可以查看 PKGBUILD"),
        ("Do you want to remove ALL files from cache?", "要删除缓存中的所有文件吗？"),
        ("Do you want to remove unused repositories?", "要删除不再使用的仓库数据库吗？"),
        ("Do you want to clean ALL AUR packages from cache?", "要清理缓存中的全部 AUR 软件包吗？"),
        ("Do you want to clean all other AUR packages from cache?", "要清理缓存中的其他 AUR 软件包吗？"),
        ("Do you want to remove all saved diffs?", "要删除所有已保存的差异文件吗？"),
        ("Start paru -Scc? pacman will separately ask about removing cached packages and unused repository databases; paru will then ask about AUR clones and saved diffs.", "开始执行 paru -Scc？pacman 会分别询问是否删除缓存软件包和无用仓库数据库；随后 paru 会询问是否清理 AUR 克隆与已保存的差异。"),
        ("SEARCH", "搜索"),
        ("NOTICE", "通知"),
        ("ERROR", "错误"),
        ("Packages using a proxy:", "以下为采用代理的软件包："),
        ("PERSISTENT PROXY RULES", "持久代理规则"),
        ("EXECUTION", "执行状态"),
        ("SESSION ACTIVITY", "活动记录"),
        ("UPDATE ALL", "更新全部"),
        ("UPDATE REPOSITORIES", "更新官方仓库"),
        ("UPDATE AUR PACKAGE", "更新单个 AUR 包"),
        ("UPDATE AUR", "更新 AUR"),
        ("INSTALL PACKAGE", "安装软件包"),
        ("REMOVE PACKAGE", "删除软件包"),
        ("REMOVAL PLAN", "确认删除清单"),
        ("Remove the prepared packages?", "确认删除以下软件包？"),
        ("Remove only the selected package.", "仅删除当前选中的软件包。"),
        ("The final removal plan will be shown before any packages are removed.", "实际删除前将展示完整的删除清单。"),
        ("BACKEND REQUEST", "操作确认 / 输入"),
        ("DEPENDENCIES", "依赖项"),
        ("Architecture", "架构"),
        ("Licenses", "许可证"),
        ("Groups", "组"),
        ("Provides", "提供"),
        ("Optional dependencies", "可选依赖"),
        ("Conflicts with", "冲突"),
        ("Replaces", "替代"),
        ("Packager", "打包者"),
        ("Build date", "构建日期"),
        ("Install date", "安装日期"),
        ("Install reason", "安装原因"),
        ("Required by", "被依赖于"),
        ("Optional for", "可选依赖于"),
        ("Validated by", "验证方式"),
        ("Install script", "安装脚本"),
        ("Backup files", "备份文件"),
        ("None", "无"),
        ("Explicit", "显式安装"),
        ("Depend", "作为依赖安装"),
        ("Select a package to inspect.", "选择软件包查看详情。"),
        ("Loading package details…", "正在读取软件包详情…"),
        ("No matching packages", "没有匹配的软件包"),
        ("Scanning…", "正在扫描…"),
        ("not configured", "尚未配置"),
        (
            "Enter saves · Esc discards · Ctrl+U clears",
            "Enter 保存并退出 · Esc 放弃 · Ctrl+U 清空",
        ),
        (
            "host:port uses HTTP; explicit socks5h:// is supported",
            "主机:端口 默认使用 HTTP；SOCKS 请填写 socks5h://",
        ),
        (
            "Applies to Git clone/fetch/ls-remote in update workers.",
            "控制更新过程中 Git 克隆、拉取和版本查询的代理。",
        ),
        (
            "A marked package base uses its proxy for downloads",
            "标记的 pkgbase 在下载和构建时使用代理，",
        ),
        (
            "and builds, including split packages.",
            "包括同一 PKGBUILD 产生的拆分包。",
        ),
        (
            "Existing operations keep their starting settings.",
            "已启动的任务继续使用启动时的设置。",
        ),
        (
            "Press p on an AUR package to toggle its base.",
            "在 AUR 包上按 p 切换其 pkgbase 的代理规则。",
        ),
        ("Git proxy setting saved", "Git 代理设置已保存"),
        ("Language saved", "语言设置已保存"),
        ("Proceed to review?", "继续审阅构建文件？"),
        ("Accept changes?", "接受这些修改？"),
        ("Nothing to do", "无需更新"),
        ("Transaction completed", "事务已完成"),
        ("Refreshing repository databases", "正在刷新仓库数据库"),
    ];
    let trimmed = text.trim();
    for (en, zh) in labels {
        if trimmed == en {
            return text.replacen(en, zh, 1);
        }
        // Panel counts have a numeric suffix; do not translate arbitrary package content.
        if let Some(suffix) = trimmed.strip_prefix(en) {
            if !suffix.trim().is_empty() && suffix.trim().parse::<usize>().is_ok() {
                return text.replacen(en, zh, 1);
            }
        }
    }
    let phrases = [
        ("1 updates", "1 更新"),
        ("2 packages", "2 软件包"),
        ("3 settings", "3 设置"),
        ("4 activity", "4 活动"),
        ("Proxy URL", "代理地址"),
        ("Git network proxy:", "Git 网络代理:"),
        ("Language / 语言:", "语言 / Language:"),
        ("Theme color:", "主题色:"),
        ("Theme color saved", "主题色已保存"),
        ("(package base)", "(软件包基底)"),
        ("(Git follows settings)", "(Git 遵循设置)"),
        (" · EDITING", " · 编辑中"),
        (
            "Settings saved · proxy address accepted · ↑↓ select a setting",
            "设置已保存 · ↑↓ 选择设置项",
        ),
        ("Proxy not saved:", "代理未保存:"),
        (
            "Enter save & finish  Esc discard  Ctrl+U clear  host:port defaults to HTTP",
            "Enter 保存并退出  Esc 放弃  Ctrl+U 清空  主机:端口 默认 HTTP",
        ),
        (
            "↑↓ setting  Enter edit/toggle  Git changes save immediately",
            "↑↓ 选择设置  Enter 编辑/切换  开关和语言立即保存",
        ),
        (
            "←→ collection  ↑↓ package  / search  Enter install  p proxy  Ctrl+↑↓ details",
            "←→ 切换目录  ↑↓ 选择包  / 搜索  Enter 安装  p 代理  Ctrl+↑↓ 滚动详情",
        ),
        (
            "←→ source  ↑↓ package  Enter AUR package  u source update  a all  p proxy  / search",
            "Tab/Shift+Tab 来源/焦点  ↑↓ 选择包  Enter 单包更新  u 当前来源  a 全部  p 代理  / 搜索",
        ),
        (
            "Enter confirm · Esc cancel · PgUp/PgDn read plan",
            "Enter 确认 · Esc 取消 · PgUp/PgDn 查看计划",
        ),
        ("Version    ", "版本       "),
        ("Available  ", "可更新至   "),
        ("Installed  ", "已安装     "),
        ("Disk size  ", "安装大小   "),
        ("Network    ", "网络       "),
        (
            "Install the prepared transaction?",
            "确认执行已准备好的事务？",
        ),
        ("Download size:", "下载大小："),
        ("Installed size:", "安装大小："),
        ("Net change:", "净变化："),
        ("Packages to remove:", "待删除软件包："),
        ("Freed disk space:", "释放空间："),
        (
            "PgUp/PgDn build history  Ctrl+C interrupt  1–4 pages  q quit",
            "PgUp/PgDn 构建历史  Ctrl+C 中断  1–4 切换页面  q 退出",
        ),
        ("BUILD OUTPUT", "构建输出"),
        ("DOWNLOADS", "下载进度"),
        ("idle", "空闲"),
        ("[ YES ]    no", "[ 是 ]    否"),
        ("yes    [ NO ]", "是    [ 否 ]"),
    ];
    let controls = [
        (
            "Tab/Shift+Tab source/focus  ↑↓ package  Enter update  u source  a all  p proxy  1–4 pages",
            "Tab/Shift+Tab 来源/焦点  ↑↓ 选择包  Enter 单包更新  u 当前来源  a 全部  p 代理  1–4 页面",
        ),
        (
            "Tab focus  ↑↓/PgUp/PgDn build history  Home oldest  End live  Esc packages  1–4 pages",
            "Tab 焦点  ↑↓/PgUp/PgDn 构建历史  Home 最早  End 实时  Esc 列表  1–4 页面",
        ),
        (
            "Tab focus  ↑↓/PgUp/PgDn details  Esc packages  1–4 pages",
            "Tab 焦点  ↑↓/PgUp/PgDn 详情  Esc 列表  1–4 页面",
        ),
        (
            "Tab/Shift+Tab collection/focus  ↑↓ navigate  / search  Enter install  p proxy  1–4 pages",
            "Tab/Shift+Tab 目录/焦点  ↑↓ 浏览  / 搜索  Enter 安装  p 代理  1–4 页面",
        ),
        (
            "↑↓/PgUp/PgDn activity  Ctrl+C interrupt  1–4 pages  q quit",
            "↑↓/PgUp/PgDn 活动记录  Ctrl+C 中断  1–4 页面  q 退出",
        ),
    ];
    for (en, zh) in controls {
        if text.trim() == en {
            return text.replacen(en, zh, 1);
        }
    }
    let mut result = text.to_owned();
    if text.contains(" installed   │") {
        result = result
            .replace(" installed   │", " 已安装   │")
            .replace(" repository   │", " 仓库包   │")
            .replace(" AUR updates   │", " AUR 更新   │")
            .replace(" proxy rules", " 代理规则")
            .replace("● ready", "● 就绪")
            .replace("◌ scanning", "◌ 扫描中")
            .replace("! AUR incomplete", "! AUR 查询未完成")
            .replace("! repo cache", "! 仓库缓存");
    }
    for (en, zh) in phrases {
        result = result.replace(en, zh);
    }
    result
}
