use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_WINSW_PATH: &str = "winsw.exe";

/// WinSW 允许的操作列表
const ALLOWED_ACTIONS: &[&str] = &[
    "install",
    "uninstall",
    "start",
    "stop",
    "restart",
    "restart!",
    "status",
    "refresh",
];

#[derive(Debug, Error)]
pub enum WinswError {
    #[error("不支持的 WinSW 操作: {0}")]
    UnsupportedAction(String),
    #[error("命令 '{0}' 需要提供配置文件路径 (req.config)")]
    ConfigRequired(String),
    #[error("无法启动 WinSW: {0}")]
    SpawnFailed(#[from] std::io::Error),
    #[error("等待 WinSW 输出失败: {0}")]
    WaitFailed(String),
    #[error("WinSW 操作超时 ({0}s)")]
    Timeout(u64),
    #[error("配置文件不存在: {0}")]
    ConfigNotFound(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct ActionReq {
    /// WinSW 可执行文件路径，默认为 "winsw.exe"
    winsw_path: Option<String>,
    /// 配置文件路径（XML 格式）
    config: Option<String>,
    /// 超时时间（秒），默认为 30
    timeout_seconds: Option<u64>,
    /// 自定义环境变量
    env_vars: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionResp {
    ok: bool,
    stdout: Option<String>,
    stderr: Option<String>,
    code: i32,
    error: Option<String>,
}

impl ActionResp {
    fn success(stdout: Option<String>, stderr: Option<String>, code: i32) -> Self {
        Self {
            ok: true,
            stdout,
            stderr,
            code,
            error: None,
        }
    }

    fn failure(code: i32, error: String) -> Self {
        Self {
            ok: false,
            stdout: None,
            stderr: None,
            code,
            error: Some(error),
        }
    }
}

/// 检查操作是否需要配置文件
fn requires_config(action: &str) -> bool {
    matches!(
        action,
        "install" | "uninstall" | "start" | "stop" | "restart" | "restart!" | "status" | "refresh"
    )
}

/// 验证并规范化操作名称
fn validate_action(action: &str) -> Result<String, WinswError> {
    let action_lc = action.trim().to_lowercase();
    if !ALLOWED_ACTIONS.contains(&action_lc.as_str()) {
        return Err(WinswError::UnsupportedAction(action.to_string()));
    }
    Ok(action_lc)
}

/// 构建 WinSW 命令参数
fn build_command_args(action: &str, config: Option<&str>) -> Result<Vec<String>, WinswError> {
    let mut args = vec![action.to_string()];

    if requires_config(action) {
        if let Some(cfg) = config {
            // 验证配置文件是否存在
            if !Path::new(cfg).exists() {
                return Err(WinswError::ConfigNotFound(cfg.to_string()));
            }
            args.push(cfg.to_string());
        } else {
            return Err(WinswError::ConfigRequired(action.to_string()));
        }
    }

    Ok(args)
}

/// 获取增强的环境变量（合并系统环境和自定义环境）
fn get_enhanced_env(custom_env: Option<&HashMap<String, String>>) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();

    // 确保关键的系统路径存在
    if let Some(path) = env.get("PATH") {
        let mut paths: Vec<String> = path.split(';').map(|s| s.to_string()).collect();

        // 添加常见的 Windows 系统路径
        let system_paths = vec![
            r"C:\Windows\System32",
            r"C:\Windows",
            r"C:\Windows\System32\Wbem",
            r"C:\Windows\System32\WindowsPowerShell\v1.0",
        ];

        for sys_path in system_paths {
            if !paths.iter().any(|p| p.eq_ignore_ascii_case(sys_path)) {
                paths.push(sys_path.to_string());
            }
        }

        env.insert("PATH".to_string(), paths.join(";"));
    }

    // 确保 TEMP 和 TMP 环境变量存在
    if !env.contains_key("TEMP") {
        if let Ok(temp) = std::env::var("TEMP") {
            env.insert("TEMP".to_string(), temp);
        }
    }
    if !env.contains_key("TMP") {
        if let Ok(tmp) = std::env::var("TMP") {
            env.insert("TMP".to_string(), tmp);
        }
    }

    // 确保 SystemRoot 存在
    if !env.contains_key("SystemRoot") {
        env.insert("SystemRoot".to_string(), r"C:\Windows".to_string());
    }

    // 合并自定义环境变量（会覆盖现有值）
    if let Some(custom) = custom_env {
        for (key, value) in custom {
            env.insert(key.clone(), value.clone());
        }
    }

    env
}

/// 读取进程输出流
async fn read_output(mut stream: impl tokio::io::AsyncRead + Unpin) -> Option<String> {
    let mut buf = Vec::new();
    match stream.read_to_end(&mut buf).await {
        Ok(_) if !buf.is_empty() => Some(String::from_utf8_lossy(&buf).to_string()),
        _ => None,
    }
}

/// 执行 WinSW 操作的核心逻辑
async fn execute_winsw(
    winsw_path: &str,
    action: &str,
    config: Option<&str>,
    timeout_secs: u64,
    custom_env: Option<&HashMap<String, String>>,
) -> Result<ActionResp, WinswError> {
    // 构建命令参数
    let args = build_command_args(action, config)?;

    // 获取增强的环境变量
    let env = get_enhanced_env(custom_env);

    // 启动 WinSW 进程
    let mut child = Command::new(winsw_path)
        .args(&args)
        .envs(&env)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(WinswError::from)?;

    // 等待进程退出，带超时控制
    let status = match timeout(Duration::from_secs(timeout_secs), child.wait()).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(WinswError::WaitFailed(e.to_string())),
        Err(_) => {
            // 超时，强制终止进程
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(WinswError::Timeout(timeout_secs));
        }
    };

    // 读取输出
    let stdout = if let Some(s) = child.stdout.take() {
        read_output(s).await
    } else {
        None
    };

    let stderr = if let Some(s) = child.stderr.take() {
        read_output(s).await
    } else {
        None
    };

    let ok = status.success();
    let code = status.code().unwrap_or(if ok { 0 } else { -1 });

    Ok(ActionResp::success(stdout, stderr, code))
}

/// Tauri 命令：执行 WinSW 操作
///
/// # 参数
/// - `action`: WinSW 操作名称（install, uninstall, start, stop, restart, restart!, status, refresh）
/// - `req`: 可选的请求参数，包含：
///   - `winsw_path`: WinSW 可执行文件路径（默认: "winsw.exe"）
///   - `config`: 配置文件路径（XML 格式）
///   - `timeout_seconds`: 超时时间（秒，默认: 30）
///   - `env_vars`: 自定义环境变量
///
/// # 返回
/// 返回操作结果，包括是否成功、标准输出、标准错误、退出码和错误信息
///
/// # 示例
/// ```javascript
/// // 前端调用示例 - 启动服务
/// await invoke('winsw_action', {
///   action: 'start',
///   req: {
///     config: 'C:\\myapp\\service.xml',
///     timeout_seconds: 60
///   }
/// });
///
/// // 前端调用示例 - 安装服务（带自定义环境变量）
/// await invoke('winsw_action', {
///   action: 'install',
///   req: {
///     config: 'C:\\myapp\\service.xml',
///     env_vars: {
///       'APP_HOME': 'C:\\myapp',
///       'LOG_LEVEL': 'INFO'
///     }
///   }
/// });
/// ```
#[tauri::command]
pub async fn winsw_action(action: String, req: Option<ActionReq>) -> Result<ActionResp, String> {
    // 验证操作名称
    let action_lc = match validate_action(&action) {
        Ok(a) => a,
        Err(e) => return Ok(ActionResp::failure(-1, e.to_string())),
    };

    // 解析请求参数
    let winsw_path = req
        .as_ref()
        .and_then(|r| r.winsw_path.as_deref())
        .unwrap_or(DEFAULT_WINSW_PATH);

    let timeout_secs = req
        .as_ref()
        .and_then(|r| r.timeout_seconds)
        .unwrap_or(DEFAULT_TIMEOUT_SECS);

    let config = req.as_ref().and_then(|r| r.config.as_deref());

    let custom_env = req.as_ref().and_then(|r| r.env_vars.as_ref());

    // 执行 WinSW 操作
    match execute_winsw(winsw_path, &action_lc, config, timeout_secs, custom_env).await {
        Ok(resp) => Ok(resp),
        Err(e) => Ok(ActionResp::failure(-1, e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_action() {
        assert!(validate_action("start").is_ok());
        assert!(validate_action("START").is_ok());
        assert!(validate_action("  stop  ").is_ok());
        assert!(validate_action("invalid").is_err());
    }

    #[test]
    fn test_requires_config() {
        assert!(requires_config("install"));
        assert!(requires_config("start"));
        assert!(requires_config("status"));
    }

    #[test]
    fn test_get_enhanced_env() {
        let custom = HashMap::from([
            ("CUSTOM_VAR".to_string(), "value".to_string()),
        ]);
        let env = get_enhanced_env(Some(&custom));

        assert!(env.contains_key("CUSTOM_VAR"));
        assert!(env.contains_key("PATH"));
        assert!(env.contains_key("SystemRoot"));
    }

    #[tokio::test]
    async fn test_action_resp_construction() {
        let resp = ActionResp::success(Some("ok".into()), None, 0);
        assert!(resp.ok);
        assert_eq!(resp.code, 0);

        let resp = ActionResp::failure(-1, "error".into());
        assert!(!resp.ok);
        assert_eq!(resp.code, -1);
    }

    #[test]
    fn test_build_command_args() {
        // 需要配置的操作
        let args = build_command_args("start", Some("test.xml"));
        assert!(args.is_err()); // 因为 test.xml 文件不存在

        // 需要配置但未提供
        let args = build_command_args("start", None);
        assert!(args.is_err());
    }
}
