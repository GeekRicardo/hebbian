//! 临时调试 example：扫 / 端到端导入 skill 仓库，用于验真实仓库布局。
//!
//! - `cargo run --example scan_skill_dir -p agent-core -- scan <path>`
//! - `cargo run --example scan_skill_dir -p agent-core -- gh <repo-url> [subpath] [rel1,rel2,...]`

use agent_core::storage::skills::{
    import_from_github, scan_skill_dir, scan_skill_github, ImportScope,
};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("scan");
    match mode {
        "scan" => {
            let path = args.get(1).expect("usage: scan <path>");
            let path = std::path::PathBuf::from(path);
            let result = scan_skill_dir(&path).expect("scan_skill_dir");
            print_scan(&path.display().to_string(), &result);
        }
        "gh" => {
            let repo = args.get(1).expect("usage: gh <repo-url> [subpath]");
            let subpath = args.get(2).map(String::as_str);
            let result = scan_skill_github(repo, subpath).expect("scan_skill_github");
            print_scan(repo, &result);

            // 第 4 个参数：要导入的 relative_path 列表（逗号分隔）
            if let Some(selected) = args.get(3) {
                let tmp = std::env::temp_dir()
                    .join(format!("hebbian-example-data-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&tmp).unwrap();
                let sel: Vec<String> = selected.split(',').map(String::from).collect();
                println!("\n--- import to {tmp:?} with selection: {sel:?}");
                match import_from_github(
                    &tmp,
                    ImportScope::Global,
                    None,
                    repo,
                    subpath,
                    Some(&sel),
                    true,
                ) {
                    Ok(imported) => {
                        println!("imported {} skill(s):", imported.len());
                        for i in imported {
                            println!(
                                "  - name={:<25} overwritten={} dest={}",
                                i.name,
                                i.overwritten,
                                i.dest.display()
                            );
                        }
                    }
                    Err(e) => eprintln!("import failed: {e}"),
                }
            }
        }
        other => eprintln!("unknown mode: {other}"),
    }
}

fn print_scan(label: &str, result: &[agent_core::storage::skills::ScannedSkill]) {
    println!("found {} skills under {}:", result.len(), label);
    for s in result {
        let desc = if s.description.len() > 60 {
            format!("{}…", &s.description[..60])
        } else {
            s.description.clone()
        };
        println!(
            "  - name={:<35} rel={:<40} desc={}",
            s.name, s.relative_path, desc
        );
    }
}
