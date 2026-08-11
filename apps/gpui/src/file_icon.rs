//! 文件树里按扩展名挑图标。
//!
//! 与原前端 `FileIcon.tsx` 同一张表、同一套 codicon 子类型图标（file-code /
//! file-text / file-media / file-binary / file-pdf / file-zip）。原版这么做是为了
//! 接近 VS Code 默认文件图标主题的观感——统一用一个通用文件图标的话，
//! 文件树扫一眼分不出源码、图片还是压缩包。

use crate::assets::Icon;

/// 整个文件名就能定性的那几个（没有扩展名）。
fn by_whole_name(name: &str) -> Option<Icon> {
    match name.to_ascii_lowercase().as_str() {
        "license" | "makefile" | "dockerfile" | "procfile" => Some(Icon::CoFileCode),
        "gitignore" | "gitattributes" | "editorconfig" | "env" | "gemfile" | "brewfile" => {
            Some(Icon::CoFileText)
        }
        _ => None,
    }
}

/// 文件图标。先按扩展名查表，查不到再看整名，都不中就是通用文件图标。
pub fn file_icon(name: &str) -> Icon {
    let lower = name.to_ascii_lowercase();
    // `.gitignore` 这种以点开头的，扩展名就是去掉点之后的整段。
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "7z" => Icon::CoFileZip,
        "astro" => Icon::CoFileCode,
        "bash" => Icon::CoFileCode,
        "bmp" => Icon::CoFileMedia,
        "bz2" => Icon::CoFileZip,
        "c" => Icon::CoFileCode,
        "cfg" => Icon::CoFileText,
        "cjs" => Icon::CoFileCode,
        "class" => Icon::CoFileBinary,
        "cmake" => Icon::CoFileCode,
        "conf" => Icon::CoFileText,
        "cpp" => Icon::CoFileCode,
        "cs" => Icon::CoFileCode,
        "css" => Icon::CoFileCode,
        "csv" => Icon::CoFileText,
        "dart" => Icon::CoFileCode,
        "deb" => Icon::CoFileBinary,
        "dll" => Icon::CoFileBinary,
        "dmg" => Icon::CoFileBinary,
        "dockerfile" => Icon::CoFileCode,
        "dylib" => Icon::CoFileBinary,
        "editorconfig" => Icon::CoFileText,
        "env" => Icon::CoFileText,
        "ex" => Icon::CoFileCode,
        "exe" => Icon::CoFileBinary,
        "exs" => Icon::CoFileCode,
        "fish" => Icon::CoFileCode,
        "gif" => Icon::CoFileMedia,
        "gitattributes" => Icon::CoFileText,
        "gitignore" => Icon::CoFileText,
        "go" => Icon::CoFileCode,
        "gz" => Icon::CoFileZip,
        "h" => Icon::CoFileCode,
        "hpp" => Icon::CoFileCode,
        "hs" => Icon::CoFileCode,
        "htm" => Icon::CoFileCode,
        "html" => Icon::CoFileCode,
        "ico" => Icon::CoFileMedia,
        "ini" => Icon::CoFileText,
        "iso" => Icon::CoFileBinary,
        "java" => Icon::CoFileCode,
        "jpeg" => Icon::CoFileMedia,
        "jpg" => Icon::CoFileMedia,
        "js" => Icon::CoFileCode,
        "json" => Icon::CoFileCode,
        "jsonc" => Icon::CoFileCode,
        "jsx" => Icon::CoFileCode,
        "kt" => Icon::CoFileCode,
        "less" => Icon::CoFileCode,
        "license" => Icon::CoFileText,
        "lock" => Icon::CoFileText,
        "log" => Icon::CoFileText,
        "lua" => Icon::CoFileCode,
        "makefile" => Icon::CoFileCode,
        "markdown" => Icon::CoFileText,
        "md" => Icon::CoFileText,
        "mdx" => Icon::CoFileText,
        "mjs" => Icon::CoFileCode,
        "mp3" => Icon::CoFileMedia,
        "mp4" => Icon::CoFileMedia,
        "nix" => Icon::CoFileText,
        "ogg" => Icon::CoFileMedia,
        "otf" => Icon::CoFileMedia,
        "pdf" => Icon::CoFilePdf,
        "php" => Icon::CoFileCode,
        "png" => Icon::CoFileMedia,
        "ps1" => Icon::CoFileCode,
        "py" => Icon::CoFileCode,
        "pyc" => Icon::CoFileBinary,
        "r" => Icon::CoFileCode,
        "rar" => Icon::CoFileZip,
        "rb" => Icon::CoFileCode,
        "rpm" => Icon::CoFileBinary,
        "rs" => Icon::CoFileCode,
        "sass" => Icon::CoFileCode,
        "scala" => Icon::CoFileCode,
        "scss" => Icon::CoFileCode,
        "sh" => Icon::CoFileCode,
        "so" => Icon::CoFileBinary,
        "sql" => Icon::CoFileCode,
        "svelte" => Icon::CoFileCode,
        "svg" => Icon::CoFileMedia,
        "swift" => Icon::CoFileCode,
        "tar" => Icon::CoFileZip,
        "tgz" => Icon::CoFileZip,
        "tiff" => Icon::CoFileMedia,
        "toml" => Icon::CoFileCode,
        "ts" => Icon::CoFileCode,
        "tsv" => Icon::CoFileText,
        "tsx" => Icon::CoFileCode,
        "ttf" => Icon::CoFileMedia,
        "txt" => Icon::CoFileText,
        "vue" => Icon::CoFileCode,
        "wasm" => Icon::CoFileBinary,
        "wav" => Icon::CoFileMedia,
        "webm" => Icon::CoFileMedia,
        "webp" => Icon::CoFileMedia,
        "woff" => Icon::CoFileMedia,
        "woff2" => Icon::CoFileMedia,
        "xml" => Icon::CoFileCode,
        "xz" => Icon::CoFileZip,
        "yaml" => Icon::CoFileCode,
        "yml" => Icon::CoFileCode,
        "zig" => Icon::CoFileCode,
        "zip" => Icon::CoFileZip,
        "zsh" => Icon::CoFileCode,
        "zst" => Icon::CoFileZip,
        _ => by_whole_name(lower.trim_start_matches('.')).unwrap_or(Icon::CoFile),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_by_extension() {
        assert_eq!(file_icon("main.rs"), Icon::CoFileCode);
        assert_eq!(file_icon("README.md"), Icon::CoFileText);
        assert_eq!(file_icon("logo.png"), Icon::CoFileMedia);
        assert_eq!(file_icon("bundle.zip"), Icon::CoFileZip);
    }

    /// 没有扩展名的那几个也要认出来——`Dockerfile` / `.gitignore` 在真实仓库里很常见，
    /// 全落到通用图标上文件树就花了。
    #[test]
    fn maps_by_whole_name() {
        assert_eq!(file_icon("Dockerfile"), Icon::CoFileCode);
        assert_eq!(file_icon(".gitignore"), Icon::CoFileText);
        assert_eq!(file_icon("LICENSE"), Icon::CoFileCode);
    }

    #[test]
    fn unknown_falls_back_to_plain_file() {
        assert_eq!(file_icon("whatever.xyz"), Icon::CoFile);
        assert_eq!(file_icon("noext"), Icon::CoFile);
    }
}
