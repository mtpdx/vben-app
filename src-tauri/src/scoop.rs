use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::process::Command;
use tokio::sync::RwLock;
use tokio::time::timeout;

/// Scoop 包管理封装模块
///
/// 提供 Scoop 的检测、安装与卸载能力，并以异步 API 暴露，同时提供 Tauri 命令以便前端调用。
///
/// 使用示例：
///
/// ```no_run
/// use app_lib::scoop::{is_scoop_installed, install_package, uninstall_package, InstallOptions};
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let installed = is_scoop_installed().await.unwrap_or(false);
/// if installed {
///     let _ = install_package("python", InstallOptions::default()).await;
///     let _ = uninstall_package("python", false, Default::default()).await;
/// }
/// # });
/// ```
///
/// 限制与已知问题：
/// - 仅在 Windows 上工作，且依赖 PowerShell 可用。
/// - 安装/卸载操作真实执行系统命令，请在受控环境下使用。
pub mod api {
    use super::*;

    const DEFAULT_TIMEOUT_SECS: u64 = 600;
    const BOOTSTRAP_TIMEOUT_SECS: u64 = 120;
    const VERSION_CHECK_TIMEOUT_SECS: u64 = 10;
    const CACHE_TTL_SECS: u64 = 60;

    /// 安装参数
    #[derive(Debug, Clone, Default, Deserialize)]
    pub struct InstallOptions {
        /// 超时时间（秒）
        pub timeout_seconds: Option<u64>,
        /// 是否为全局安装（需要管理员权限）
        pub global: Option<bool>,
        /// 仅构建命令，不执行（用于测试/基准）
        pub dry_run: Option<bool>,
        /// 附加参数（如 `--arch 64bit` 等）
        pub extra_args: Option<Vec<String>>,
    }

    /// 操作统一响应
    #[derive(Debug, Clone, Serialize)]
    pub struct ActionResp {
        pub ok: bool,
        pub stdout: Option<String>,
        pub stderr: Option<String>,
        pub code: i32,
        pub error: Option<String>,
    }

    /// 检测响应
    #[derive(Debug, Clone, Serialize)]
    pub struct DetectResp {
        pub installed: bool,
        pub version: Option<String>,
        pub error: Option<String>,
        pub source: Option<String>,
        pub cached: bool,
    }

    /// 引导安装选项
    #[derive(Debug, Clone, Default, Deserialize)]
    pub struct BootstrapOptions {
        pub timeout_seconds: Option<u64>,
        pub dry_run: Option<bool>,
    }

    /// 模块错误类型
    #[derive(Debug, Error)]
    pub enum ScoopError {
        #[error("PowerShell 不可用: {0}")]
        PowerShellNotAvailable(String),
        #[error("命令启动失败: {0}")]
        CommandSpawn(#[from] std::io::Error),
        #[error("命令执行超时: {secs}s")]
        Timeout { secs: u64 },
        #[error("命令执行失败，退出码: {code:?}, 错误: {stderr}")]
        CommandFailed { code: Option<i32>, stderr: String },
        #[error("包名无效或为空")]
        InvalidPackageName,
    }

    #[derive(Debug, Clone)]
    struct DetectionCache {
        last_check: Instant,
        installed: bool,
        version: Option<String>,
    }

    // 使用 Arc<RwLock> 替代 OnceLock<Mutex>，提供更好的并发性能
    static DETECT_CACHE: tokio::sync::OnceCell<Arc<RwLock<Option<DetectionCache>>>> =
        tokio::sync::OnceCell::const_new();

    async fn get_cache() -> &'static Arc<RwLock<Option<DetectionCache>>> {
        DETECT_CACHE
            .get_or_init(|| async { Arc::new(RwLock::new(None)) })
            .await
    }

    async fn cache_get() -> Option<DetectResp> {
        let cache = get_cache().await;
        let guard = cache.read().await;

        if let Some(c) = guard.as_ref() {
            if c.last_check.elapsed() <= Duration::from_secs(CACHE_TTL_SECS) {
                return Some(DetectResp {
                    installed: c.installed,
                    version: c.version.clone(),
                    error: None,
                    source: Some("cache".into()),
                    cached: true,
                });
            }
        }
        None
    }

    async fn cache_put(installed: bool, version: Option<String>) {
        let cache = get_cache().await;
        let mut guard = cache.write().await;
        *guard = Some(DetectionCache {
            last_check: Instant::now(),
            installed,
            version,
        });
    }

    fn powershell_path() -> Option<PathBuf> {
        which::which("pwsh.exe")
            .or_else(|_| which::which("powershell.exe"))
            .ok()
    }

    fn build_ps_command_args(script: &str) -> Vec<String> {
        vec![
            "-NoProfile".to_string(),
            "-ExecutionPolicy".to_string(),
            "Bypass".to_string(),
            "-Command".to_string(),
            script.to_string(),
        ]
    }

    /// 获取增强的环境变量（包含 Scoop 路径）
    fn get_enhanced_env() -> HashMap<String, String> {
        let mut env: HashMap<String, String> = std::env::vars().collect();

        // 确保 SCOOP 相关路径在 PATH 中
        if let Some(path) = env.get("PATH") {
            let mut paths: Vec<String> = path.split(';').map(|s| s.to_string()).collect();

            // 添加用户 Scoop 路径
            if let Ok(userprofile) = std::env::var("USERPROFILE") {
                let scoop_shims = format!("{}\\scoop\\shims", userprofile);
                let scoop_apps = format!("{}\\scoop\\apps\\scoop\\current\\bin", userprofile);

                if !paths.iter().any(|p| p.contains("scoop\\shims")) {
                    paths.insert(0, scoop_shims);
                }
                if !paths.iter().any(|p| p.contains("scoop\\apps")) {
                    paths.insert(0, scoop_apps);
                }
            }

            // 添加全局 Scoop 路径
            if let Ok(programdata) = std::env::var("ProgramData") {
                let global_shims = format!("{}\\scoop\\shims", programdata);
                if !paths.iter().any(|p| p.contains("ProgramData\\scoop")) {
                    paths.insert(0, global_shims);
                }
            }

            env.insert("PATH".to_string(), paths.join(";"));
        }

        // 设置 SCOOP 环境变量
        if let Ok(homedrive) = std::env::var("HOMEDRIVE") {
            env.insert("SCOOP".to_string(), format!("{}\\aidex\\scoop", homedrive));
        } else if let Ok(userprofile) = std::env::var("USERPROFILE") {
            env.insert(
                "SCOOP".to_string(),
                format!("{}\\aidex\\scoop", userprofile),
            );
        } else {
            env.insert("SCOOP".to_string(), "C:\\aidex\\scoop".into());
        }

        // 设置 SCOOP_GLOBAL 环境变量
        if let Ok(programdata) = std::env::var("ProgramData") {
            env.insert(
                "SCOOP_GLOBAL".to_string(),
                format!("{}\\aidex\\scoop", programdata),
            );
        } else {
            env.insert("SCOOP_GLOBAL".to_string(), "C:\\aidex\\scoop".into());
        }

        env
    }

    /// 检测 Scoop 是否安装
    pub async fn is_scoop_installed() -> Result<bool, ScoopError> {
        // 先检查缓存
        if let Some(cached) = cache_get().await {
            return Ok(cached.installed);
        }

        // 检查 scoop 命令是否在 PATH 中
        if which::which("scoop").is_ok() {
            let ver = try_scoop_version().await.ok();
            cache_put(true, ver).await;
            return Ok(true);
        }

        // 检查 Scoop 安装目录
        let home = std::env::var_os("SCOOP")
            .or_else(|| std::env::var_os("SCOOP_HOME"))
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("USERPROFILE").map(|up| {
                    let mut p = PathBuf::from(up);
                    p.push("scoop");
                    p
                })
            });

        let installed = home.map(|h| h.exists()).unwrap_or(false);
        cache_put(installed, None).await;
        Ok(installed)
    }

    async fn try_scoop_version() -> Result<String, ScoopError> {
        let ps = powershell_path().ok_or_else(|| {
            ScoopError::PowerShellNotAvailable("未找到 PowerShell 可执行文件".into())
        })?;

        let args = build_ps_command_args("scoop --version");
        let env = get_enhanced_env();

        let child = Command::new(ps)
            .args(&args)
            .envs(&env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let out = timeout(
            Duration::from_secs(VERSION_CHECK_TIMEOUT_SECS),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| ScoopError::Timeout {
            secs: VERSION_CHECK_TIMEOUT_SECS,
        })??;

        if out.status.success() {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            Ok(version)
        } else {
            Err(ScoopError::CommandFailed {
                code: out.status.code(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            })
        }
    }

    /// 获取 Scoop 版本字符串（若命令可用）
    pub async fn scoop_version() -> Result<String, ScoopError> {
        try_scoop_version().await
    }

    /// 返回当前检测缓存快照（若仍在 TTL 内）
    pub async fn detection_cache() -> Option<DetectResp> {
        cache_get().await
    }

    /// 安装 Scoop（运行执行策略与安装脚本），支持 dry_run
    pub async fn install_scoop(opts: BootstrapOptions) -> Result<ActionResp, ScoopError> {
        let ps = powershell_path().ok_or_else(|| {
            ScoopError::PowerShellNotAvailable("未找到 PowerShell 可执行文件".into())
        })?;

        let timeout_secs = opts.timeout_seconds.unwrap_or(BOOTSTRAP_TIMEOUT_SECS);
        let dry_run = opts.dry_run.unwrap_or(false);

        let set_policy =
            "Set-ExecutionPolicy -ExecutionPolicy RemoteSigned -Scope CurrentUser -Force";
        let install_cmd = "Invoke-RestMethod -Uri https://get.scoop.sh | Invoke-Expression";

        if dry_run {
            let combined = format!("{}\n{}", set_policy, install_cmd);
            return Ok(ActionResp {
                ok: true,
                stdout: Some(combined),
                stderr: None,
                code: 0,
                error: None,
            });
        }

        let env = get_enhanced_env();

        // 设置执行策略
        let out1 = execute_ps_command(&ps, set_policy, timeout_secs, &env).await?;
        if !out1.status.success() {
            return Err(ScoopError::CommandFailed {
                code: out1.status.code(),
                stderr: String::from_utf8_lossy(&out1.stderr).to_string(),
            });
        }

        // 运行安装脚本
        let out2 = execute_ps_command(&ps, install_cmd, timeout_secs, &env).await?;
        let ok = out2.status.success();
        let stdout = parse_output(&out2.stdout);
        let stderr = parse_output(&out2.stderr);
        let code = out2.status.code().unwrap_or(if ok { 0 } else { -1 });

        if ok {
            let ver = try_scoop_version().await.ok();
            cache_put(true, ver).await;
            Ok(ActionResp {
                ok,
                stdout,
                stderr,
                code,
                error: None,
            })
        } else {
            Err(ScoopError::CommandFailed {
                code: out2.status.code(),
                stderr: stderr.unwrap_or_default(),
            })
        }
    }

    /// 确保 Scoop 已安装：若未安装则自动安装，返回检测信息
    pub async fn ensure_scoop_installed(opts: BootstrapOptions) -> Result<DetectResp, ScoopError> {
        if is_scoop_installed().await? {
            let cached = detection_cache().await.is_some();
            return Ok(DetectResp {
                installed: true,
                version: try_scoop_version().await.ok(),
                error: None,
                source: Some("detect".into()),
                cached,
            });
        }

        let res = install_scoop(opts).await?;
        if res.ok {
            Ok(DetectResp {
                installed: true,
                version: try_scoop_version().await.ok(),
                error: None,
                source: Some("bootstrap".into()),
                cached: false,
            })
        } else {
            Err(ScoopError::CommandFailed {
                code: Some(res.code),
                stderr: res.stderr.unwrap_or_default(),
            })
        }
    }

    /// 安装包
    pub async fn install_package(
        pkg: &str,
        opts: InstallOptions,
    ) -> Result<ActionResp, ScoopError> {
        let pkg = pkg.trim();
        if pkg.is_empty() {
            return Err(ScoopError::InvalidPackageName);
        }

        let timeout_secs = opts.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECS);
        let global = opts.global.unwrap_or(false);
        let dry_run = opts.dry_run.unwrap_or(false);
        let extra_args = opts.extra_args.unwrap_or_default();

        let ps = powershell_path().ok_or_else(|| {
            ScoopError::PowerShellNotAvailable("未找到 PowerShell 可执行文件".into())
        })?;

        // 构建命令
        let mut cmd_parts = vec!["scoop install"];
        if global {
            cmd_parts.push("--global");
        }
        cmd_parts.push(pkg);

        let cmdline = if extra_args.is_empty() {
            cmd_parts.join(" ")
        } else {
            format!("{} {}", cmd_parts.join(" "), extra_args.join(" "))
        };

        if dry_run {
            return Ok(ActionResp {
                ok: true,
                stdout: Some(cmdline),
                stderr: None,
                code: 0,
                error: None,
            });
        }

        let env = get_enhanced_env();
        let out = execute_ps_command(&ps, &cmdline, timeout_secs, &env).await?;
        let ok = out.status.success();

        if ok {
            Ok(ActionResp {
                ok,
                stdout: parse_output(&out.stdout),
                stderr: parse_output(&out.stderr),
                code: out.status.code().unwrap_or(0),
                error: None,
            })
        } else {
            Err(ScoopError::CommandFailed {
                code: out.status.code(),
                stderr: parse_output(&out.stderr).unwrap_or_default(),
            })
        }
    }

    /// 卸载包
    pub async fn uninstall_package(
        pkg: &str,
        purge: bool,
        opts: InstallOptions,
    ) -> Result<ActionResp, ScoopError> {
        let pkg = pkg.trim();
        if pkg.is_empty() {
            return Err(ScoopError::InvalidPackageName);
        }

        let timeout_secs = opts.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECS);
        let dry_run = opts.dry_run.unwrap_or(false);

        let ps = powershell_path().ok_or_else(|| {
            ScoopError::PowerShellNotAvailable("未找到 PowerShell 可执行文件".into())
        })?;

        let cmdline = if purge {
            format!("scoop uninstall --purge {}", pkg)
        } else {
            format!("scoop uninstall {}", pkg)
        };

        if dry_run {
            return Ok(ActionResp {
                ok: true,
                stdout: Some(cmdline),
                stderr: None,
                code: 0,
                error: None,
            });
        }

        let env = get_enhanced_env();
        let out = execute_ps_command(&ps, &cmdline, timeout_secs, &env).await?;
        let ok = out.status.success();

        if ok {
            Ok(ActionResp {
                ok,
                stdout: parse_output(&out.stdout),
                stderr: parse_output(&out.stderr),
                code: out.status.code().unwrap_or(0),
                error: None,
            })
        } else {
            Err(ScoopError::CommandFailed {
                code: out.status.code(),
                stderr: parse_output(&out.stderr).unwrap_or_default(),
            })
        }
    }

    // 辅助函数：执行 PowerShell 命令
    async fn execute_ps_command(
        ps_path: &PathBuf,
        script: &str,
        timeout_secs: u64,
        env: &HashMap<String, String>,
    ) -> Result<std::process::Output, ScoopError> {
        let args = build_ps_command_args(script);
        let child = Command::new(ps_path)
            .args(&args)
            .envs(env)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
            .await
            .map_err(|_| ScoopError::Timeout { secs: timeout_secs })?
            .map_err(ScoopError::from)
    }

    // 辅助函数：解析输出
    fn parse_output(output: &[u8]) -> Option<String> {
        if output.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(output).to_string())
        }
    }
}

pub use api::*;

#[derive(Deserialize)]
pub struct InstallReq {
    pub package: String,
    pub global: Option<bool>,
    pub timeout_seconds: Option<u64>,
    pub dry_run: Option<bool>,
    pub extra_args: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct DetectCmdResp {
    pub ok: bool,
    pub installed: bool,
    pub version: Option<String>,
    pub error: Option<String>,
    pub cached: bool,
}

/// Tauri 命令：Scoop 检测
#[tauri::command]
pub async fn scoop_detect() -> Result<DetectCmdResp, String> {
    match is_scoop_installed().await {
        Ok(installed) => {
            let v = scoop_version().await.ok();
            let cached = detection_cache().await.is_some();
            Ok(DetectCmdResp {
                ok: true,
                installed,
                version: v,
                error: None,
                cached,
            })
        }
        Err(e) => Ok(DetectCmdResp {
            ok: false,
            installed: false,
            version: None,
            error: Some(e.to_string()),
            cached: false,
        }),
    }
}

/// Tauri 命令：安装包
#[tauri::command]
pub async fn scoop_install(req: InstallReq) -> Result<ActionResp, String> {
    let opts = InstallOptions {
        timeout_seconds: req.timeout_seconds,
        global: req.global,
        dry_run: req.dry_run,
        extra_args: req.extra_args,
    };
    match install_package(&req.package, opts).await {
        Ok(r) => Ok(r),
        Err(e) => Ok(ActionResp {
            ok: false,
            stdout: None,
            stderr: None,
            code: -1,
            error: Some(e.to_string()),
        }),
    }
}

/// Tauri 命令：卸载包
#[tauri::command]
pub async fn scoop_uninstall(
    package: String,
    purge: Option<bool>,
    timeout_seconds: Option<u64>,
    dry_run: Option<bool>,
) -> Result<ActionResp, String> {
    let opts = InstallOptions {
        timeout_seconds,
        global: None,
        dry_run,
        extra_args: None,
    };
    match uninstall_package(&package, purge.unwrap_or(false), opts).await {
        Ok(r) => Ok(r),
        Err(e) => Ok(ActionResp {
            ok: false,
            stdout: None,
            stderr: None,
            code: -1,
            error: Some(e.to_string()),
        }),
    }
}

/// Tauri 命令：确保 Scoop 已安装（未安装则执行安装脚本）
#[tauri::command]
pub async fn scoop_ensure(
    dry_run: Option<bool>,
    timeout_seconds: Option<u64>,
) -> Result<DetectCmdResp, String> {
    let opts = BootstrapOptions {
        timeout_seconds,
        dry_run,
    };
    match ensure_scoop_installed(opts).await {
        Ok(d) => Ok(DetectCmdResp {
            ok: true,
            installed: d.installed,
            version: d.version,
            error: None,
            cached: d.cached,
        }),
        Err(e) => Ok(DetectCmdResp {
            ok: false,
            installed: false,
            version: None,
            error: Some(e.to_string()),
            cached: false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_install_uninstall_dry_run() {
        let r = install_package(
            "python",
            InstallOptions {
                dry_run: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(r.ok);
        assert!(r.stdout.unwrap().contains("scoop install"));

        let r2 = uninstall_package(
            "python",
            true,
            InstallOptions {
                dry_run: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(r2.ok);
        assert!(r2.stdout.unwrap().contains("scoop uninstall"));
    }

    #[tokio::test]
    async fn test_invalid_pkg() {
        let e = install_package("  ", Default::default())
            .await
            .err()
            .unwrap();
        match e {
            ScoopError::InvalidPackageName => (),
            _ => panic!("unexpected error"),
        }
    }

    #[tokio::test]
    async fn test_cache_flow() {
        let _ = is_scoop_installed().await;
        let c = detection_cache().await;
        assert!(c.is_some());
        let c2 = detection_cache().await;
        assert!(c2.unwrap().cached);
    }

    #[tokio::test]
    async fn test_install_scoop_dry_run() {
        let r = install_scoop(BootstrapOptions {
            dry_run: Some(true),
            timeout_seconds: Some(1),
        })
        .await
        .unwrap();
        assert!(r.ok);
        let out = r.stdout.unwrap();
        assert!(out.contains("Set-ExecutionPolicy"));
        assert!(out.contains("Invoke-RestMethod"));
    }
}
