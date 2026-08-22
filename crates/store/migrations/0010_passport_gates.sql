-- 0010_passport_gates: 通关文牒的六关摘要子表
CREATE TABLE passport_gates (
    passport_id TEXT NOT NULL REFERENCES passports(id),
    seq INTEGER NOT NULL,
    gate TEXT NOT NULL,
    passed INTEGER NOT NULL,
    evidence_ids TEXT NOT NULL DEFAULT '[]',
    failed_inputs TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (passport_id, seq)
);
