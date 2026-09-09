-- 0060_skill_market_public_sources: 技能市场内置默认源改公网地址。
-- 原内置源扫本机 ZCode 插件缓存（~/.zcode/cli/plugins），现改为各自公网来源：
--   zcode-plugins-official → 远程清单源（https 拉取 marketplace.json，插件按 zip+sha256 下载校验）；
--   claude-plugins-official → 远程 Git 源（GitHub 市场仓库）。
-- 仅重写两条未改动过的内置源（kind+root_path+marketplace_id 同时命中），
-- 用户自建/已改过的源不受影响；revision+1 使既有 CAS 编辑令牌失效。
UPDATE skill_market_sources SET
  kind = 'remote_url',
  root_path = 'https://cdn-zcode.z.ai/zcode/official-plugin/marketplace.json',
  revision = revision + 1,
  updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE kind = 'zcode_local'
  AND root_path = '~/.zcode/cli/plugins'
  AND marketplace_id = 'zcode-plugins-official';

UPDATE skill_market_sources SET
  kind = 'remote_git',
  root_path = 'https://github.com/anthropics/claude-plugins-official',
  revision = revision + 1,
  updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
WHERE kind = 'zcode_local'
  AND root_path = '~/.zcode/cli/plugins'
  AND marketplace_id = 'claude-plugins-official';
