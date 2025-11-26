use app_lib::scoop::{install_package, uninstall_package, is_scoop_installed, ensure_scoop_installed, api::InstallOptions, api::BootstrapOptions};

#[tokio::test]
async fn integration_detect_and_dry_runs() {
    let _ = is_scoop_installed().await; // should not panic

    let resp = install_package("git", InstallOptions { dry_run: Some(true), ..Default::default() }).await.unwrap();
    assert!(resp.ok);
    assert!(resp.stdout.unwrap().contains("scoop install"));

    let resp2 = uninstall_package("git", false, InstallOptions { dry_run: Some(true), ..Default::default() }).await.unwrap();
    assert!(resp2.ok);
    assert!(resp2.stdout.unwrap().contains("scoop uninstall"));

    let _ = ensure_scoop_installed(BootstrapOptions { dry_run: Some(true), timeout_seconds: Some(1) }).await.unwrap();
}