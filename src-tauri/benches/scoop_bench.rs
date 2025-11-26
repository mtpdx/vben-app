use app_lib::scoop::api::InstallOptions;
use app_lib::scoop::install_package;
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_build_install_cmd(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    c.bench_function("build_install_cmd_git", |b| {
        b.iter(|| {
            let _ = rt.block_on(async {
                install_package("git", InstallOptions { dry_run: Some(true), ..Default::default() }).await.unwrap()
            });
        });
    });
}

criterion_group!(benches, bench_build_install_cmd);
criterion_main!(benches);