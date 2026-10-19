//! Interface text, in Simplified Chinese and English.
//!
//! Both the engine's controls and the launcher's settings page read their
//! labels from here, so a new string is added once and the two languages can
//! never drift apart silently: [`Text`] has one field per string and both
//! tables fill every field, which the compiler enforces.
//!
//! The launcher's settings page draws the same tables through a CJK-capable
//! Windows font (Microsoft YaHei UI), so both languages render there without
//! bundling a font.

use crate::preferences::{Language, Resolved};

/// Every user-visible string the two windows draw.
#[derive(Clone, Copy, Debug)]
pub struct Text {
    // Engine controls.
    pub controls_subtitle: &'static str,
    pub hit_regions: &'static str,
    pub hit_regions_hint: &'static str,
    pub hit_regions_detail: &'static str,
    pub add_2d: &'static str,
    pub add_3d: &'static str,
    pub characters: &'static str,
    pub position: &'static str,
    pub responding_to: &'static str,
    pub no_interaction: &'static str,
    pub reload_selected: &'static str,
    pub remove_selected: &'static str,
    pub local_path: &'static str,
    pub load_as_new: &'static str,
    pub replace_selected: &'static str,
    pub drop_hint: &'static str,
    pub dismiss_error: &'static str,
    pub quit: &'static str,
    pub library: &'static str,
    pub add_from_library: &'static str,
    pub library_user: &'static str,
    pub library_hint: &'static str,
    pub wander: &'static str,
    pub wander_hint: &'static str,
    pub status_loaded: &'static str,
    pub status_added: &'static str,
    pub status_loading_model: &'static str,
    pub status_no_model: &'static str,
    pub frame_stats: &'static str,
    // Chat panel.
    pub chat: &'static str,
    pub chat_placeholder: &'static str,
    pub chat_send: &'static str,
    pub chat_disabled: &'static str,
    pub chat_local: &'static str,
    pub chat_remote: &'static str,
    pub chat_waiting: &'static str,
    pub chat_target: &'static str,
    pub chat_clear: &'static str,
    pub chat_select_target: &'static str,
    pub chat_empty: &'static str,
    pub chat_you: &'static str,
    pub chat_system: &'static str,
    // Launcher settings page.
    pub settings_title: &'static str,
    pub launcher_version: &'static str,
    pub engine_version: &'static str,
    pub engine_missing: &'static str,
    pub last_check: &'static str,
    pub last_check_never: &'static str,
    pub checking: &'static str,
    pub updates_disabled: &'static str,
    pub no_check_yet: &'static str,
    pub check_updates: &'static str,
    pub install_restart: &'static str,
    pub open_logs: &'static str,
    pub quit_pet: &'static str,
    pub close: &'static str,
    pub language: &'static str,
    pub language_system: &'static str,
    pub default_character: &'static str,
    pub chat_section: &'static str,
    pub chat_enabled: &'static str,
    pub chat_base_url: &'static str,
    pub chat_model: &'static str,
    pub chat_api_key: &'static str,
    pub chat_save: &'static str,
    pub chat_saved: &'static str,
    pub chat_requires_https: &'static str,
    pub chat_incomplete: &'static str,
    pub settings_error: &'static str,
    pub log_events: &'static str,
    pub toggle_controls: &'static str,
    pub open_settings: &'static str,
}

/// English, the fallback for any language this build does not translate.
///
/// A `static`, not a `const`: `for_language` hands out references, and a
/// `const` would be duplicated at every use site, so two callers could see
/// two different tables.
pub static EN: Text = Text {
    controls_subtitle: "Native 2D / 3D desktop characters",
    hit_regions: "Character hit regions",
    hit_regions_hint: "Scene input is limited to the projected character outlines, \
padded by 8 px per axis; empty areas stay click-through. Off: the whole scene window \
intercepts input. The control window never becomes click-through.",
    hit_regions_detail: "Character regions + 8px; off: full scene.",
    add_2d: "Add 2D",
    add_3d: "Add 3D",
    characters: "Characters",
    position: "Position",
    responding_to: "Responding to character",
    no_interaction: "No nearby interaction",
    reload_selected: "Reload selected",
    remove_selected: "Remove selected",
    local_path: "Local .dmodel.json path",
    load_as_new: "Load as new",
    replace_selected: "Replace selected",
    drop_hint: "Dropping a model file on either window adds a character.",
    dismiss_error: "Dismiss error",
    quit: "Quit",
    library: "Model library",
    add_from_library: "Add",
    library_user: "user",
    library_hint: "Place .dmodel.json files in %APPDATA%\\Delicium\\models to add them here.",
    wander: "Autonomous wandering",
    wander_hint: "Characters walk along the visible desktop on their own; a drag or a position edit pauses them.",
    status_loaded: "Loaded {name}",
    status_added: "Added {name}",
    status_loading_model: "Loading {name}",
    status_no_model: "No library model named `{id}`",
    frame_stats: "Frame {frame:.2} ms | tick {tick} | interactions {interactions} | pending {pending}",
    chat: "Conversation",
    chat_placeholder: "Say something to the character...",
    chat_send: "Send",
    chat_disabled: "Remote chat is off; the character answers with local lines.",
    chat_local: "local",
    chat_remote: "remote",
    chat_waiting: "Waiting for a reply...",
    chat_target: "Talking to",
    chat_clear: "Clear",
    chat_select_target: "Select a character to open its conversation.",
    chat_empty: "No messages yet.",
    chat_you: "You",
    chat_system: "System",
    settings_title: "Delicium Settings",
    launcher_version: "Delicium launcher",
    engine_version: "Engine",
    engine_missing: "not installed",
    last_check: "Last check",
    last_check_never: "Last check: never",
    checking: "Checking for updates...",
    updates_disabled: "Update checks are disabled (--no-update).",
    no_check_yet: "No update check has run yet.",
    check_updates: "Check for updates",
    install_restart: "Install and restart",
    open_logs: "Open logs",
    quit_pet: "Quit pet",
    close: "Close",
    language: "Language",
    language_system: "System default",
    default_character: "Default character",
    chat_section: "Conversation (OpenAI-compatible)",
    chat_enabled: "Enable remote conversations",
    chat_base_url: "Base URL (https://... or http://127.0.0.1:...)",
    chat_model: "Model name",
    chat_api_key: "API key",
    chat_save: "Save conversation settings",
    chat_saved: "Saved.",
    chat_requires_https: "Remote addresses must use https; http is only allowed for localhost.",
    chat_incomplete: "Enter a base URL and a model name, or turn remote conversations off.",
    settings_error: "Cannot save the setting:",
    log_events: "Log events",
    toggle_controls: "Show/Hide controls",
    open_settings: "Settings...",
};

/// Simplified Chinese.
pub static ZH_HANS: Text = Text {
    controls_subtitle: "原生 2D / 3D 桌宠",
    hit_regions: "角色命中区域",
    hit_regions_hint: "场景输入只落在角色投影轮廓（每轴外扩 8 像素）上，空白处保持穿透；\
关闭后整个场景窗口都会拦截输入。控制窗本身永不穿透。",
    hit_regions_detail: "角色轮廓 + 8 像素；关闭后为整窗。",
    add_2d: "添加 2D",
    add_3d: "添加 3D",
    characters: "角色",
    position: "位置",
    responding_to: "正在回应角色",
    no_interaction: "附近没有互动",
    reload_selected: "重新加载所选",
    remove_selected: "移除所选",
    local_path: "本地 .dmodel.json 路径",
    load_as_new: "作为新角色加载",
    replace_selected: "替换所选角色",
    drop_hint: "把模型文件拖到任一窗口即可添加角色。",
    dismiss_error: "清除错误",
    quit: "退出",
    library: "模型库",
    add_from_library: "添加",
    library_user: "用户",
    library_hint: "把 .dmodel.json 放进 %APPDATA%\\Delicium\\models 就会出现在这里。",
    wander: "自主漫游",
    wander_hint: "角色会沿着可见桌面自行走动；拖拽或修改坐标会暂停它。",
    status_loaded: "已加载 {name}",
    status_added: "已添加 {name}",
    status_loading_model: "正在加载 {name}",
    status_no_model: "模型库中没有 `{id}`",
    frame_stats: "帧 {frame:.2} 毫秒 | 步进 {tick} | 互动 {interactions} | 等待 {pending}",
    chat: "对话",
    chat_placeholder: "和角色说点什么……",
    chat_send: "发送",
    chat_disabled: "远程对话未启用，角色会用本地台词回应。",
    chat_local: "本地",
    chat_remote: "远程",
    chat_waiting: "正在等待回复……",
    chat_target: "对话对象",
    chat_clear: "清空",
    chat_select_target: "选择一个角色后即可打开它的对话。",
    chat_empty: "还没有消息。",
    chat_you: "你",
    chat_system: "系统",
    settings_title: "Delicium 设置",
    launcher_version: "Delicium 启动器",
    engine_version: "引擎",
    engine_missing: "未安装",
    last_check: "上次检查",
    last_check_never: "上次检查：从未",
    checking: "正在检查更新……",
    updates_disabled: "更新检查已禁用（--no-update）。",
    no_check_yet: "尚未执行过更新检查。",
    check_updates: "检查更新",
    install_restart: "安装并重启",
    open_logs: "打开日志",
    quit_pet: "退出桌宠",
    close: "关闭",
    language: "语言",
    language_system: "跟随系统",
    default_character: "默认角色",
    chat_section: "对话（OpenAI 兼容）",
    chat_enabled: "启用远程对话",
    chat_base_url: "接口地址（https://... 或 http://127.0.0.1:...）",
    chat_model: "模型名称",
    chat_api_key: "API 密钥",
    chat_save: "保存对话设置",
    chat_saved: "已保存。",
    chat_requires_https: "远程地址必须使用 https；http 仅允许本机回环地址。",
    chat_incomplete: "请填写接口地址和模型名称，或关闭远程对话。",
    settings_error: "无法保存设置：",
    log_events: "日志事件",
    toggle_controls: "显示/隐藏控制窗",
    open_settings: "设置……",
};

/// Fills `{name}` (and only that placeholder) in a template.
pub fn named(template: &str, name: &str) -> String {
    template.replace("{name}", name)
}

/// The table for a resolved language.
pub fn text(resolved: Resolved) -> &'static Text {
    match resolved {
        Resolved::En => &EN,
        Resolved::ZhHans => &ZH_HANS,
    }
}

/// The table for a language setting, resolving `System`.
pub fn for_language(language: Language) -> &'static Text {
    text(language.resolved())
}

/// Local character lines, used when no remote endpoint is configured or it
/// fails. Indexed by a caller-chosen counter so replies vary but stay
/// deterministic for a given scene.
#[derive(Clone, Copy, Debug)]
pub struct LocalLines {
    pub greeting: &'static [&'static str],
    pub play: &'static [&'static str],
    pub reply: &'static [&'static str],
}

pub const LOCAL_EN: LocalLines = LocalLines {
    greeting: &["Hello there!", "Oh, a friend!", "Nice to see you."],
    play: &["That was fun!", "Again, again!", "Tag, you are it!"],
    reply: &[
        "I am listening.",
        "Tell me more.",
        "Hmm, I see.",
        "Interesting!",
    ],
};

pub const LOCAL_ZH_HANS: LocalLines = LocalLines {
    greeting: &["你好呀！", "哦，是朋友！", "见到你真高兴。"],
    play: &["真好玩！", "再来一次！", "抓到你了！"],
    reply: &["我在听。", "再多说一点。", "嗯，我明白了。", "有意思！"],
};

pub fn local_lines(resolved: Resolved) -> &'static LocalLines {
    match resolved {
        Resolved::En => &LOCAL_EN,
        Resolved::ZhHans => &LOCAL_ZH_HANS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_languages_fill_every_string() {
        for text in [&EN, &ZH_HANS] {
            for value in [
                text.controls_subtitle,
                text.hit_regions,
                text.hit_regions_hint,
                text.hit_regions_detail,
                text.add_2d,
                text.add_3d,
                text.characters,
                text.position,
                text.responding_to,
                text.no_interaction,
                text.reload_selected,
                text.remove_selected,
                text.local_path,
                text.load_as_new,
                text.replace_selected,
                text.drop_hint,
                text.dismiss_error,
                text.quit,
                text.library,
                text.add_from_library,
                text.library_user,
                text.library_hint,
                text.wander,
                text.wander_hint,
                text.status_loaded,
                text.status_added,
                text.status_loading_model,
                text.status_no_model,
                text.frame_stats,
                text.chat,
                text.chat_placeholder,
                text.chat_send,
                text.chat_disabled,
                text.chat_local,
                text.chat_remote,
                text.chat_waiting,
                text.chat_target,
                text.chat_clear,
                text.chat_select_target,
                text.chat_empty,
                text.chat_you,
                text.chat_system,
                text.settings_title,
                text.launcher_version,
                text.engine_version,
                text.engine_missing,
                text.last_check,
                text.last_check_never,
                text.checking,
                text.updates_disabled,
                text.no_check_yet,
                text.check_updates,
                text.install_restart,
                text.open_logs,
                text.quit_pet,
                text.close,
                text.language,
                text.language_system,
                text.default_character,
                text.chat_section,
                text.chat_enabled,
                text.chat_base_url,
                text.chat_model,
                text.chat_api_key,
                text.chat_save,
                text.chat_saved,
                text.chat_requires_https,
                text.chat_incomplete,
                text.settings_error,
                text.log_events,
                text.toggle_controls,
                text.open_settings,
            ] {
                assert!(!value.trim().is_empty());
            }
        }
    }

    #[test]
    fn chinese_and_english_actually_differ() {
        assert_ne!(EN.quit, ZH_HANS.quit);
        assert_ne!(EN.settings_title, ZH_HANS.settings_title);
        assert_ne!(EN.chat_send, ZH_HANS.chat_send);
        // The frame counter keeps all four placeholders in both languages.
        for template in [EN.frame_stats, ZH_HANS.frame_stats] {
            for key in ["{frame", "{tick", "{interactions", "{pending"] {
                assert!(template.contains(key), "{template}");
            }
        }
    }

    #[test]
    fn local_lines_are_nonempty_for_both_languages() {
        for lines in [&LOCAL_EN, &LOCAL_ZH_HANS] {
            for bank in [lines.greeting, lines.play, lines.reply] {
                assert!(!bank.is_empty());
                assert!(bank.iter().all(|line| !line.trim().is_empty()));
            }
        }
    }

    #[test]
    fn the_selection_resolves_to_a_table() {
        assert!(std::ptr::eq(for_language(Language::En), &EN));
        assert!(std::ptr::eq(for_language(Language::ZhHans), &ZH_HANS));
        // System resolves to one of the two known tables, never a panic.
        let system = for_language(Language::System);
        assert!(std::ptr::eq(system, &EN) || std::ptr::eq(system, &ZH_HANS));
    }
}
