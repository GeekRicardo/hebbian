//! 微信 iLink 扫码登录。

use std::path::Path;

use qrcode::render::unicode;
use qrcode::QrCode;
use reqwest::Client;

use super::types::{BotCredentials, QrCodeResponse, QrCodeStatus, QrLoginStatus};

const BASE_URL: &str = "https://ilinkai.weixin.qq.com";

/// 申请一张登录二维码。返回 `(qrcode_id, content)`：
/// - `qrcode_id` 用于后续轮询扫码状态
/// - `content` 是要渲染成二维码图案的字符串（GUI 端渲染图片，CLI 端渲染终端 ASCII）
pub async fn request_qrcode() -> anyhow::Result<(String, String)> {
    let http = Client::new();
    let qr: QrCodeResponse = http
        .get(format!("{BASE_URL}/ilink/bot/get_bot_qrcode?bot_type=3"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok((qr.qrcode, qr.qrcode_img_content))
}

/// 轮询一次扫码状态。GUI 每隔几秒调一次推进登录状态机。
pub async fn poll_qrcode_status(qrcode_id: &str) -> anyhow::Result<QrLoginStatus> {
    let http = Client::new();
    let status: QrCodeStatus = http
        .get(format!(
            "{BASE_URL}/ilink/bot/get_qrcode_status?qrcode={qrcode_id}"
        ))
        .header("iLink-App-ClientVersion", "1")
        .timeout(std::time::Duration::from_secs(40))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    match status.status.as_str() {
        "scaned" => Ok(QrLoginStatus::Scanned),
        "confirmed" => {
            let credentials = BotCredentials {
                bot_token: status.bot_token.unwrap_or_default(),
                bot_id: status.ilink_bot_id.unwrap_or_default(),
                user_id: status.ilink_user_id.unwrap_or_default(),
            };
            if credentials.bot_token.is_empty() || credentials.bot_id.is_empty() {
                anyhow::bail!("登录响应缺少 bot_token 或 bot_id");
            }
            Ok(QrLoginStatus::Confirmed(credentials))
        }
        "expired" => Ok(QrLoginStatus::Expired),
        _ => Ok(QrLoginStatus::Waiting),
    }
}

/// CLI 扫码登录：申请二维码 → 终端渲染 → 阻塞轮询直到确认 / 过期。
pub async fn login() -> anyhow::Result<BotCredentials> {
    let (qrcode_id, content) = request_qrcode().await?;
    print_qr_to_terminal(&content);
    eprintln!("请用微信扫描上方二维码登录...");

    loop {
        match poll_qrcode_status(&qrcode_id).await? {
            QrLoginStatus::Scanned => eprintln!("已扫码，请在手机上确认..."),
            QrLoginStatus::Confirmed(credentials) => {
                eprintln!("✅ 登录成功！bot_id={}", credentials.bot_id);
                return Ok(credentials);
            }
            QrLoginStatus::Expired => anyhow::bail!("二维码已过期，请重新运行登录"),
            QrLoginStatus::Waiting => {}
        }
    }
}

fn print_qr_to_terminal(content: &str) {
    eprintln!("═══════════════════════════════════════");
    eprintln!("  微信扫码登录");
    match render_qr(content) {
        Some(rendered) => eprintln!("{rendered}"),
        None => eprintln!("  二维码渲染失败，请手动打开下方链接："),
    }
    eprintln!("  扫不出来时复制此链接到浏览器：");
    eprintln!("  {content}");
    eprintln!("═══════════════════════════════════════");
}

/// 把内容渲染成深色终端可扫的 Unicode 二维码（dark/light 反色：边框为实心块、数据区留白）。
fn render_qr(content: &str) -> Option<String> {
    let code = QrCode::new(content).ok()?;
    Some(
        code.render::<unicode::Dense1x2>()
            .dark_color(unicode::Dense1x2::Light)
            .light_color(unicode::Dense1x2::Dark)
            .quiet_zone(true)
            .build(),
    )
}

/// 把内容渲染成 SVG 字符串，供 GUI 直接 `<img>` / inline 显示，前端无需二维码库。
pub fn render_qr_svg(content: &str) -> anyhow::Result<String> {
    let code = QrCode::new(content)?;
    Ok(code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(220, 220)
        .quiet_zone(true)
        .build())
}

pub fn save_credentials(data_dir: &Path, credentials: &BotCredentials) -> anyhow::Result<()> {
    let dir = data_dir
        .join("channels")
        .join("wechat")
        .join(&credentials.bot_id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(
        dir.join("credentials.json"),
        serde_json::to_string_pretty(credentials)?,
    )?;
    Ok(())
}

pub fn load_credentials(data_dir: &Path, bot_id: &str) -> anyhow::Result<BotCredentials> {
    let path = data_dir
        .join("channels")
        .join("wechat")
        .join(bot_id)
        .join("credentials.json");
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_qr_produces_scannable_block_art() {
        // 登录的命门：之前那版只打印 URL，用户根本扫不了码。这条测试钉住二维码必须真渲染。
        let rendered = render_qr("https://ilinkai.weixin.qq.com/qr/abc123").expect("应成功渲染");
        // 反色配置下数据/边框用实心块 █，多行结构完整。
        assert!(rendered.contains('█'), "渲染结果应包含二维码实心块");
        assert!(
            rendered.lines().count() > 10,
            "二维码应是多行图案，实得 {} 行",
            rendered.lines().count()
        );
    }
}
