---
name: warn-pk-access
description: HL agent wallet PK へのアクセスを試みた場合に warn する (PreToolUse deny を抜けた場合の二次防御)
event: PreToolUse
match: 'pass show diff-old-new/hl/agent-pk|agent-pk\.gpg|\.password-store/diff-old-new|HL_AGENT_PK=|source .*scripts/load-env\.sh'
severity: warn
---

# HL agent wallet PK access detected

`.claude/hooks/deny-pk-access.sh` で deny されるはずの操作が観測された.
hook が無効化されている可能性があるので, settings.json を確認すること.

PK は `~/.password-store/diff-old-new/hl/agent-pk.gpg` に GPG 暗号化保管されており,
Claude が transcript に流出させないよう project CLAUDE.md で禁止している.

PK が必要な操作はユーザー本人が別ターミナルで実行する.
