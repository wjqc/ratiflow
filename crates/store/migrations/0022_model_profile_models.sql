-- 0022_model_profile_models: model_profiles 增加同步模型列表持久化。
-- modelProfile.syncModels 的结果此前只存前端 state（刷新即丢），
-- 落库后供设置页「点模型行设默认」与工作台 Composer 模型选择器枚举可选模型。

ALTER TABLE model_profiles ADD COLUMN models_json TEXT NOT NULL DEFAULT '[]';
