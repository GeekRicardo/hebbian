//! 微信 iLink 扫码登录。

use std::path::Path;

use reqwest::Client;

use super::types::{BotCredentials, QrCodeResponse, QrCodeStatus};

const BASE_URL: &str = "https://ilinkai.weixin.qq.com";

pub async fn login() -> anyhow::Result<BotCredentials> {
    let http = Client::new();

    let qr: QrCodeResponse = http
        .get(format!("{BASE_URL}/ilink/bot/get_bot_qrcode?bot_type=3"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    print_qr_to_terminal(&qr.qrcode_img_content);
    eprintln!("请用微信扫描上方二维码登录...");

    loop {
        let status: QrCodeStatus = http
            .get(format!(
                "{BASE_URL}/ilink/bot/get_qrcode_status?qrcode={}",
                qr.qrcode
            ))
            .header("iLink-App-ClientVersion", "1")
            .timeout(std::time::Duration::from_secs(40))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        match status.status.as_str() {
            "scaned" => eprintln!("已扫码，请在手机上确认..."),
            "confirmed" => {
                let credentials = BotCredentials {
                    bot_token: status.bot_token.unwrap_or_default(),
                    bot_id: status.ilink_bot_id.unwrap_or_default(),
                    user_id: status.ilink_user_id.unwrap_or_default(),
                };
                if credentials.bot_token.is_empty() || credentials.bot_id.is_empty() {
                    anyhow::bail!("登录响应缺少 bot_token 或 bot_id");
                }
                eprintln!("✅ 登录成功！bot_id={}", credentials.bot_id);
                return Ok(credentials);
            }
            "expired" => anyhow::bail!("二维码已过期，请重新运行登录"),
            _ => {}
        }
    }
}

fn print_qr_to_terminal(url: &str) {
    eprintln!("═══════════════════════════════════════");
    eprintln!("  微信扫码登录");
    eprintln!("  如果终端无法显示二维码，请打开：");
    eprintln!("  {url}");
    eprintln!("═══════════════════════════════════════");
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
