-- 0047：workitem 判重检索（EvoFlow WP-11 A6；flag RATIFLOW_WORKITEM_FTS，默认 0）。
-- FTS5 trigram：中文子串检索；索引维护走应用层（create/update 挂钩 + searchRebuild
-- 全量回填），表本身无条件创建（纯 schema，flag 关闭时无人读写）。

CREATE VIRTUAL TABLE workitem_search USING fts5(
    workitem_id UNINDEXED,
    title,
    description,
    tokenize = 'trigram'
);
