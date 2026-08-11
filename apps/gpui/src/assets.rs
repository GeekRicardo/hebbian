//! 图标资源：SVG 以 `include_str!` 编进二进制，运行时不读磁盘。
//!
//! 这套图标与原 Web 前端用的 lucide 同名同形——重画一遍是因为 gpui 需要
//! 自带 `AssetSource`，而 gpui-component 本身不打包 icons 目录。
//!
//! gpui 把 SVG 光栅化成 alpha 遮罩后用 `text_color` 上色，所以描边式图标
//! 直接可用，SVG 里的 `stroke="currentColor"` 只是占位。

use std::borrow::Cow;

use anyhow::Result;
use gpui::{svg, AssetSource, SharedString, Styled, Svg};

/// 全部内嵌图标：路径（无扩展名）→ SVG 源码。
const ICONS: &[(&str, &str)] = &[
    ("icons/codicon/checklist", include_str!("../assets/icons/codicon/checklist.svg")),
    ("icons/codicon/comment-discussion", include_str!("../assets/icons/codicon/comment-discussion.svg")),
    ("icons/codicon/diff-modified", include_str!("../assets/icons/codicon/diff-modified.svg")),
    ("icons/codicon/files", include_str!("../assets/icons/codicon/files.svg")),
    ("icons/codicon/globe", include_str!("../assets/icons/codicon/globe.svg")),
    ("icons/codicon/list-tree", include_str!("../assets/icons/codicon/list-tree.svg")),
    ("icons/codicon/server-process", include_str!("../assets/icons/codicon/server-process.svg")),
    ("icons/codicon/source-control", include_str!("../assets/icons/codicon/source-control.svg")),
    ("icons/codicon/terminal", include_str!("../assets/icons/codicon/terminal.svg")),
    ("icons/codicon/file", include_str!("../assets/icons/codicon/file.svg")),
    ("icons/codicon/file-code", include_str!("../assets/icons/codicon/file-code.svg")),
    ("icons/codicon/file-text", include_str!("../assets/icons/codicon/file-text.svg")),
    ("icons/codicon/file-media", include_str!("../assets/icons/codicon/file-media.svg")),
    ("icons/codicon/file-binary", include_str!("../assets/icons/codicon/file-binary.svg")),
    ("icons/codicon/file-pdf", include_str!("../assets/icons/codicon/file-pdf.svg")),
    ("icons/codicon/file-zip", include_str!("../assets/icons/codicon/file-zip.svg")),
    ("icons/codicon/folder", include_str!("../assets/icons/codicon/folder.svg")),
    ("icons/codicon/folder-opened", include_str!("../assets/icons/codicon/folder-opened.svg")),
    ("icons/arrow-up", include_str!("../assets/icons/arrow-up.svg")),
    ("icons/arrow-up-from-line", include_str!("../assets/icons/arrow-up-from-line.svg")),
    ("icons/ban", include_str!("../assets/icons/ban.svg")),
    ("icons/bot", include_str!("../assets/icons/bot.svg")),
    ("icons/braces", include_str!("../assets/icons/braces.svg")),
    ("icons/brain", include_str!("../assets/icons/brain.svg")),
    ("icons/check", include_str!("../assets/icons/check.svg")),
    ("icons/chevron-down", include_str!("../assets/icons/chevron-down.svg")),
    ("icons/chevron-left", include_str!("../assets/icons/chevron-left.svg")),
    ("icons/chevron-right", include_str!("../assets/icons/chevron-right.svg")),
    ("icons/circle-check", include_str!("../assets/icons/circle-check.svg")),
    ("icons/clock", include_str!("../assets/icons/clock.svg")),
    ("icons/code-2", include_str!("../assets/icons/code-2.svg")),
    ("icons/copy", include_str!("../assets/icons/copy.svg")),
    ("icons/edit-3", include_str!("../assets/icons/edit-3.svg")),
    ("icons/file", include_str!("../assets/icons/file.svg")),
    ("icons/file-text", include_str!("../assets/icons/file-text.svg")),
    ("icons/folder", include_str!("../assets/icons/folder.svg")),
    ("icons/folder-open", include_str!("../assets/icons/folder-open.svg")),
    ("icons/gauge", include_str!("../assets/icons/gauge.svg")),
    ("icons/git-branch", include_str!("../assets/icons/git-branch.svg")),
    ("icons/globe", include_str!("../assets/icons/globe.svg")),
    ("icons/grip-vertical", include_str!("../assets/icons/grip-vertical.svg")),
    ("icons/import", include_str!("../assets/icons/import.svg")),
    ("icons/list", include_str!("../assets/icons/list.svg")),
    ("icons/list-todo", include_str!("../assets/icons/list-todo.svg")),
    ("icons/loader-circle", include_str!("../assets/icons/loader-circle.svg")),
    ("icons/message-circle", include_str!("../assets/icons/message-circle.svg")),
    ("icons/message-square", include_str!("../assets/icons/message-square.svg")),
    ("icons/message-square-plus", include_str!("../assets/icons/message-square-plus.svg")),
    ("icons/minus", include_str!("../assets/icons/minus.svg")),
    ("icons/package", include_str!("../assets/icons/package.svg")),
    ("icons/palette", include_str!("../assets/icons/palette.svg")),
    ("icons/panel-right-close", include_str!("../assets/icons/panel-right-close.svg")),
    ("icons/panel-right-open", include_str!("../assets/icons/panel-right-open.svg")),
    ("icons/pencil", include_str!("../assets/icons/pencil.svg")),
    ("icons/plug", include_str!("../assets/icons/plug.svg")),
    ("icons/plus", include_str!("../assets/icons/plus.svg")),
    ("icons/refresh-cw", include_str!("../assets/icons/refresh-cw.svg")),
    ("icons/scroll-text", include_str!("../assets/icons/scroll-text.svg")),
    ("icons/search", include_str!("../assets/icons/search.svg")),
    ("icons/server", include_str!("../assets/icons/server.svg")),
    ("icons/settings", include_str!("../assets/icons/settings.svg")),
    ("icons/shield", include_str!("../assets/icons/shield.svg")),
    ("icons/slash", include_str!("../assets/icons/slash.svg")),
    ("icons/sparkles", include_str!("../assets/icons/sparkles.svg")),
    ("icons/square", include_str!("../assets/icons/square.svg")),
    ("icons/target", include_str!("../assets/icons/target.svg")),
    ("icons/terminal", include_str!("../assets/icons/terminal.svg")),
    ("icons/trash-2", include_str!("../assets/icons/trash-2.svg")),
    ("icons/user", include_str!("../assets/icons/user.svg")),
    ("icons/user-cog", include_str!("../assets/icons/user-cog.svg")),
    ("icons/x", include_str!("../assets/icons/x.svg")),
    ("icons/zap", include_str!("../assets/icons/zap.svg")),
];

/// gpui 的资源入口。路径形如 `icons/folder-open.svg`，与 `Icon::path()` 的约定一致。
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let key = path.strip_suffix(".svg").unwrap_or(path);
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, body)| Cow::Borrowed(body.as_bytes())))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(format!("{name}.svg")))
            .collect())
    }
}

/// 应用内用到的全部图标。与原前端 `lucide-react` 的 import 列表一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    ArrowUp,
    ArrowUpFromLine,
    CoChecklist,
    CoCommentDiscussion,
    CoDiffModified,
    CoFiles,
    CoGlobe,
    CoListTree,
    CoServerProcess,
    CoSourceControl,
    CoTerminal,
    CoFile,
    CoFileCode,
    CoFileText,
    CoFileMedia,
    CoFileBinary,
    CoFilePdf,
    CoFileZip,
    CoFolder,
    CoFolderOpened,
    Ban,
    Bot,
    Braces,
    Check,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    CircleCheck,
    Clock,
    Code2,
    Copy,
    Edit3,
    File,
    FileText,
    Folder,
    FolderOpen,
    Gauge,
    GitBranch,
    Globe,
    GripVertical,
    Import,
    List,
    ListTodo,
    LoaderCircle,
    MessageSquare,
    MessageSquarePlus,
    Minus,
    Palette,
    PanelRightClose,
    PanelRightOpen,
    Pencil,
    Plus,
    RefreshCw,
    Search,
    Brain,
    MessageCircle,
    Package,
    Plug,
    ScrollText,
    Server,
    Settings,
    Slash,
    Shield,
    UserCog,
    Sparkles,
    Square,
    Target,
    Terminal,
    Trash2,
    User,
    X,
    Zap,
}

impl Icon {
    pub fn path(self) -> &'static str {
        match self {
            Icon::ArrowUp => "icons/arrow-up.svg",
            Icon::ArrowUpFromLine => "icons/arrow-up-from-line.svg",
            Icon::CoChecklist => "icons/codicon/checklist.svg",
            Icon::CoCommentDiscussion => "icons/codicon/comment-discussion.svg",
            Icon::CoDiffModified => "icons/codicon/diff-modified.svg",
            Icon::CoFiles => "icons/codicon/files.svg",
            Icon::CoGlobe => "icons/codicon/globe.svg",
            Icon::CoListTree => "icons/codicon/list-tree.svg",
            Icon::CoServerProcess => "icons/codicon/server-process.svg",
            Icon::CoSourceControl => "icons/codicon/source-control.svg",
            Icon::CoTerminal => "icons/codicon/terminal.svg",
            Icon::CoFile => "icons/codicon/file.svg",
            Icon::CoFileCode => "icons/codicon/file-code.svg",
            Icon::CoFileText => "icons/codicon/file-text.svg",
            Icon::CoFileMedia => "icons/codicon/file-media.svg",
            Icon::CoFileBinary => "icons/codicon/file-binary.svg",
            Icon::CoFilePdf => "icons/codicon/file-pdf.svg",
            Icon::CoFileZip => "icons/codicon/file-zip.svg",
            Icon::CoFolder => "icons/codicon/folder.svg",
            Icon::CoFolderOpened => "icons/codicon/folder-opened.svg",
            Icon::Ban => "icons/ban.svg",
            Icon::Bot => "icons/bot.svg",
            Icon::Braces => "icons/braces.svg",
            Icon::Check => "icons/check.svg",
            Icon::ChevronDown => "icons/chevron-down.svg",
            Icon::ChevronLeft => "icons/chevron-left.svg",
            Icon::ChevronRight => "icons/chevron-right.svg",
            Icon::CircleCheck => "icons/circle-check.svg",
            Icon::Clock => "icons/clock.svg",
            Icon::Code2 => "icons/code-2.svg",
            Icon::Copy => "icons/copy.svg",
            Icon::Edit3 => "icons/edit-3.svg",
            Icon::File => "icons/file.svg",
            Icon::FileText => "icons/file-text.svg",
            Icon::Folder => "icons/folder.svg",
            Icon::FolderOpen => "icons/folder-open.svg",
            Icon::Gauge => "icons/gauge.svg",
            Icon::GitBranch => "icons/git-branch.svg",
            Icon::Globe => "icons/globe.svg",
            Icon::GripVertical => "icons/grip-vertical.svg",
            Icon::Import => "icons/import.svg",
            Icon::List => "icons/list.svg",
            Icon::ListTodo => "icons/list-todo.svg",
            Icon::LoaderCircle => "icons/loader-circle.svg",
            Icon::MessageSquare => "icons/message-square.svg",
            Icon::MessageSquarePlus => "icons/message-square-plus.svg",
            Icon::Minus => "icons/minus.svg",
            Icon::Palette => "icons/palette.svg",
            Icon::PanelRightClose => "icons/panel-right-close.svg",
            Icon::PanelRightOpen => "icons/panel-right-open.svg",
            Icon::Pencil => "icons/pencil.svg",
            Icon::Plus => "icons/plus.svg",
            Icon::RefreshCw => "icons/refresh-cw.svg",
            Icon::Search => "icons/search.svg",
            Icon::Settings => "icons/settings.svg",
            Icon::Slash => "icons/slash.svg",
            Icon::Brain => "icons/brain.svg",
            Icon::MessageCircle => "icons/message-circle.svg",
            Icon::Package => "icons/package.svg",
            Icon::Plug => "icons/plug.svg",
            Icon::ScrollText => "icons/scroll-text.svg",
            Icon::Server => "icons/server.svg",
            Icon::Shield => "icons/shield.svg",
            Icon::UserCog => "icons/user-cog.svg",
            Icon::Sparkles => "icons/sparkles.svg",
            Icon::Square => "icons/square.svg",
            Icon::Target => "icons/target.svg",
            Icon::Terminal => "icons/terminal.svg",
            Icon::Trash2 => "icons/trash-2.svg",
            Icon::User => "icons/user.svg",
            Icon::X => "icons/x.svg",
            Icon::Zap => "icons/zap.svg",
        }
    }

    /// 渲染成一个方形 svg 元素。
    ///
    /// **颜色必须显式传**：gpui 的 svg 元素不从父容器继承 `text_color`，
    /// 漏设的后果是图标静默不显示（不是报错），所以这里把颜色做成必填参数。
    pub fn el(self, size: gpui::Pixels, color: gpui::Hsla) -> Svg {
        svg().path(self.path()).size(size).flex_none().text_color(color)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个枚举分支都必须有对应的内嵌资源，否则运行时是「图标静默不显示」这种难查的坏。
    #[test]
    fn every_icon_has_an_embedded_asset() {
        let assets = Assets;
        for icon in ALL_ICONS {
            let loaded = assets.load(icon.path()).expect("load");
            assert!(loaded.is_some(), "missing asset for {:?}", icon);
        }
    }
}

/// 测试用的全量清单。
pub const ALL_ICONS: &[Icon] = &[
    Icon::ArrowUp,
    Icon::ArrowUpFromLine,
    Icon::Ban,
    Icon::Bot,
    Icon::Braces,
    Icon::Check,
    Icon::ChevronDown,
    Icon::ChevronLeft,
    Icon::ChevronRight,
    Icon::CircleCheck,
    Icon::Clock,
    Icon::Code2,
    Icon::Copy,
    Icon::Edit3,
    Icon::File,
    Icon::FileText,
    Icon::Folder,
    Icon::FolderOpen,
    Icon::Gauge,
    Icon::GitBranch,
    Icon::Globe,
    Icon::GripVertical,
    Icon::Import,
    Icon::List,
    Icon::ListTodo,
    Icon::LoaderCircle,
    Icon::MessageSquare,
    Icon::MessageSquarePlus,
    Icon::Minus,
    Icon::Palette,
    Icon::PanelRightClose,
    Icon::PanelRightOpen,
    Icon::Pencil,
    Icon::Plus,
    Icon::RefreshCw,
    Icon::Search,
    Icon::Settings,
    Icon::Slash,
    Icon::Sparkles,
    Icon::Square,
    Icon::Target,
    Icon::Terminal,
    Icon::Trash2,
    Icon::User,
    Icon::X,
    Icon::Zap,
];
