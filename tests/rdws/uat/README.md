# RDWS UAT 记录目录

发布波次与回滚演练的用户验收记录（Runbook §5）。文件名约定 `RDWS-<ID>-<wave>-<yyyymmdd>.md`。

每条记录必含：timestamp（RFC3339）、coreVersion / schemaVersion / 数据目录 sha256、操作者、动作与结果（含原始输出摘录）、关联 CI job / pipeline 链接。

对应 `tests/rdws/manifest.json` 断言条目的 `uat.status` 由 `pending` 翻为 `recorded` 时，`record` 字段指向本目录下文件路径（`make rdws-manifest` 校验存在性）。
