use anyhow::{anyhow, Result};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

/// SMAppService はバンドルの外(cargo run)では使えないので、その場合は None を返して項目を無効にする
fn service() -> Option<objc2::rc::Retained<SMAppService>> {
    let exe = std::env::current_exe().ok()?;
    exe.to_string_lossy().contains(".app/Contents/MacOS/").then(|| unsafe { SMAppService::mainAppService() })
}

pub fn is_enabled() -> Option<bool> {
    let s = service()?;
    Some(unsafe { s.status() } == SMAppServiceStatus::Enabled)
}

pub enum Outcome {
    Enabled,
    Disabled,
    /// システム設定の「ログイン項目」で利用者の許可が要る
    NeedsApproval,
}

pub fn toggle() -> Result<Outcome> {
    let s = service().ok_or_else(|| anyhow!("DockMorph.app として起動したときだけ設定できます"))?;
    let status = unsafe { s.status() };
    if status == SMAppServiceStatus::Enabled || status == SMAppServiceStatus::RequiresApproval {
        unsafe { s.unregisterAndReturnError() }.map_err(|e| anyhow!("ログイン項目から外せません: {}", e.localizedDescription()))?;
        return Ok(Outcome::Disabled);
    }
    unsafe { s.registerAndReturnError() }.map_err(|e| anyhow!("ログイン項目に登録できません: {}", e.localizedDescription()))?;
    if unsafe { s.status() } == SMAppServiceStatus::RequiresApproval {
        unsafe { SMAppService::openSystemSettingsLoginItems() };
        return Ok(Outcome::NeedsApproval);
    }
    Ok(Outcome::Enabled)
}
