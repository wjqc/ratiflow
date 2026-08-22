//! 本地存储唯一入口：SQLite WAL（单写者）、嵌入迁移、内容寻址 objects、
//! 秘密扫描、outbox 事件与追加式审计。Rust core 是业务数据唯一写入者。

pub mod audit;
pub mod backup;
pub mod ids;
pub mod migration;
pub mod objects;
pub mod outbox;
pub mod scan;
pub mod store;
#[cfg(test)]
mod store_test;
pub mod timefmt;
pub use objects::{ObjectInfo, PutOptions};
pub use store::{Error, Store};

/// 全库统一时间戳格式（UTC 毫秒）。
pub fn now() -> String {
    timefmt::now()
}
