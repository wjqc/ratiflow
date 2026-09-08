-- 0056：workitem FTS 影子构建表（审计《Ratiflow_RDWS_v1.4实施审计与整改方案_v1.0》§8 P1-3）。
-- searchRebuild 影子切换协议：① 影子表清空+全量填充（可中断——半成品只存在于
-- 影子表，主索引不受影响，下轮重建覆盖）；② 单事务原子切换主表
-- （DELETE 主表 + INSERT..SELECT 影子）——观察者永不看到空/半索引。
-- 与 0047 主表同构（FTS5 trigram）；flag RATIFLOW_WORKITEM_FTS 语义不变。

CREATE VIRTUAL TABLE workitem_search_shadow USING fts5(
    workitem_id UNINDEXED,
    title,
    description,
    tokenize = 'trigram'
);
