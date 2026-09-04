//! feature flag 环境变量覆盖单测（M5/§16.1）——独立测试进程，
//! 避免进程级 env 与其他用例竞争。SIXGATES_MEMORY_FEATURE 仅用于开发/测试/E2E 构建；
//! 生产不设置该变量 → 走 app_settings（默认 false）。
use sg_memory as mem;
use sg_store::Store;

#[test]
fn env_override_opens_feature_without_app_settings_row() {
    // 前置：未设置变量时默认 false（同进程内验证一次基线）。
    std::env::remove_var("SIXGATES_MEMORY_FEATURE");
    let dir = std::env::temp_dir().join(format!(
        "sg-mem-env-{}-{}",
        std::process::id(),
        sg_store::ids::new_id("t")
    ));
    let store = Store::open(&dir, "test").unwrap();
    assert!(!mem::repository::feature_enabled(&store).unwrap());

    // 打开覆盖 → true（无需 app_settings 行）。
    std::env::set_var("SIXGATES_MEMORY_FEATURE", "1");
    assert!(mem::repository::feature_enabled(&store).unwrap());
    std::env::set_var("SIXGATES_MEMORY_FEATURE", "true");
    assert!(mem::repository::feature_enabled(&store).unwrap());
    // 显式关闭值 → false。
    std::env::set_var("SIXGATES_MEMORY_FEATURE", "0");
    assert!(!mem::repository::feature_enabled(&store).unwrap());
    // 清理：恢复生产语义（后续断言依赖变量不存在）。
    std::env::remove_var("SIXGATES_MEMORY_FEATURE");
    assert!(!mem::repository::feature_enabled(&store).unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}
