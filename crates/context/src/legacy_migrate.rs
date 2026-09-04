//! 存量 data migration 的启动接线（§8.2 步骤 3）：限次调用 freeze::run_legacy_migration。
use serde_json::Value;
use sg_store::Store;

/// 启动入口：单实例身份作为租约 owner。失败返回 Err（调用方决定是否阻断）。
pub fn run(store: &Store, max: usize) -> Result<Value, sg_store::Error> {
    crate::freeze::run_legacy_migration(store, max)
}
