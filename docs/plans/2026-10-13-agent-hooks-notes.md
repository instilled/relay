# User-level command hooks: OpenAI Codex CLI and Google Gemini CLI

Sources: docs pages (fetched via a summarizer, so wording is paraphrased; field names exact) plus source of
`openai/codex` main (codex-rs/hooks, core, features) and `google-gemini/gemini-cli` main (v0.65.0-nightly.20261006), cloned 2026-10-13.
Source "main" is newer than local 0.162.0 / 0.63.0; items marked [UNCERTAIN] were not verified against the exact tag.
I did not run codex/gemini beyond --version/--help.

## CODEX (local 0.162.0)

### 1. Config
- Docs: https://developers.openai.com/codex/hooks (308 -> https://learn.chatgpt.com/docs/hooks)
- Files (next to each config layer): `~/.codex/hooks.json`, `~/.codex/config.toml` (inline `[hooks]`), `<repo>/.codex/hooks.json`, `<repo>/.codex/config.toml`; plugins via hooks/hooks.json.
- Relocation: `CODEX_HOME` (must exist and be a dir) -> `$CODEX_HOME/hooks.json` | config.toml. Source: https://github.com/openai/codex/blob/main/codex-rs/utils/home-dir/src/lib.rs. The hooks doc page does not mention CODEX_HOME; the "next to each config layer" rule implies it.
- hooks.json (docs):
```json
{
  "hooks": {
    "PreToolUse": [
      { "matcher": "Bash",
        "hooks": [ { "type": "command", "command": "python3 /path/to/check.py", "timeout": 30, "statusMessage": "Checking command" } ] }
    ]
  }
}
```
- TOML (docs):
```toml
[[hooks.PreToolUse]]
matcher = "^Bash$"

[[hooks.PreToolUse.hooks]]
type = "command"
command = "python3 /path/to/check.py"
timeout = 30
```
  Multi-event: repeat the block per event name: `[[hooks.SessionStart]]`, `[[hooks.UserPromptSubmit]]`, `[[hooks.PermissionRequest]]`, `[[hooks.Stop]]`, `[[hooks.SessionEnd]]`, each with `[[hooks.X.hooks]]` `type="command"` `command="..."`.
- Handler fields: type (`command`, `mcp_tool` run; `prompt`/`agent` parsed but skipped), command, timeout (SECONDS, default 600; SessionEnd and Interrupt default 1, max 3), statusMessage, async (background, max 8 concurrent/session, cannot block), additionalContextLimit, commandWindows.
- Matcher: regex; `*`, `""` or omitted = all. Honored by PermissionRequest/PreToolUse/PostToolUse (tool name), PreCompact/PostCompact, SessionEnd (`other`), SessionStart (`startup|resume|clear|compact`), Subagent*. Ignored by UserPromptSubmit, Stop, Interrupt.
- Merge: ADDITIVE. All matching hooks from all layers run (no override); multiple matches for one event run concurrently. Project-local hooks load only if the project `.codex/` layer is trusted; user/system hooks load even in untrusted projects. If one layer has both hooks.json and inline [hooks], both merged with a startup warning.
- Feature flag: `[features] hooks` (key "hooks", Stage::Stable, default_enabled = true; https://github.com/openai/codex/blob/main/codex-rs/features/src/lib.rs). `codex_hooks` is a deprecated alias. `hooks = false` disables. [UNCERTAIN whether 0.162.0 exactly already defaults on; older builds had experimental `codex_hooks` default off. Defensive: also set `[features] hooks = true`, or check `codex features list`.]
- Trust: NON-managed hooks (including user-level) must be reviewed/trusted before running; trust is tied to the hook's hash; a changed hook is skipped until re-trusted. Review via `/hooks` in the TUI. `--dangerously-bypass-hook-trust` bypasses for one invocation. => An installer writing ~/.codex/hooks.json will NOT run until the user trusts it once in `/hooks`. Where trust is persisted: not verified [UNCERTAIN].
- Version: hooks exist in 0.162.0 (assumed; exact intro version not found) [UNCERTAIN].

### 2. Events and stdin (https://developers.openai.com/codex/hooks)
Events: PreToolUse, PermissionRequest, PostToolUse, PreCompact, PostCompact, UserPromptSubmit, SubagentStop, Stop, Interrupt, SessionStart, SubagentStart, SessionEnd.
Common stdin fields: `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `model`.
Extras per event:
- SessionStart: source (startup|resume|clear|compact), permission_mode
- UserPromptSubmit: turn_id, prompt, permission_mode
- PermissionRequest: turn_id, tool_name, tool_input (tool_input.description may be null), permission_mode
- PreToolUse: turn_id, tool_name, tool_use_id, tool_input, permission_mode
- PostToolUse: same + tool_response
- Stop: turn_id, stop_hook_active, last_assistant_message, permission_mode
- Interrupt: turn_id, permission_mode
- SessionEnd: reason
- SubagentStart/Stop, PreCompact/PostCompact: turn_id, agent_id, agent_type, trigger etc. (see doc)
permission_mode in {default, acceptEdits, plan, dontAsk, bypassPermissions}.
Mapping: started = SessionStart; working = UserPromptSubmit; waiting for approval = PermissionRequest; turn finished/idle = Stop (Interrupt = user interrupted); ended = SessionEnd (1s default timeout, max 3, so keep it fast). SessionStart may fire only when the first turn begins in some versions [UNCERTAIN].

### 3. Environment / parent
- Source: codex-rs/hooks/src/registry.rs:79 `environment = std::env::vars_os().collect()` (snapshot of the codex process env), replayed in engine/command_runner.rs build_command after `env_clear()`, filtered only of NON_INHERITABLE vars (codex exec-server noise token, NODE_REPL_AUTH_TOKEN, CODEX_GUARDIAN_DECISIONS_API_KEY, OPENAI_FEDERATION_RULE_ID, OPENAI_IDENTITY_TOKEN_FILE, OPENAI_WORKLOAD_IDENTITY_CONTEXT). So a custom env var set at launch IS visible. (Docs do not state this; source only.) https://github.com/openai/codex/blob/main/codex-rs/hooks/src/engine/command_runner.rs
- Spawn: `<shell> -lc "<command>"` (configured shell, else $SHELL, else /bin/sh), cwd = session cwd, `ProcessMode::NewSession` (setsid). Parent is the spawning shell, which may or may not exec the command, so do not rely on getppid() == codex [shell-dependent, UNCERTAIN]. Prefer a launch-time env var or session_id.

### 4. No-op
Exit 0 with no stdout = success, continue. Exit 2 blocks (PreToolUse, UserPromptSubmit, Stop, SubagentStop, PostToolUse): never use. Other nonzero or timeout = reported as a hook failure (warning, not a block). For Stop/SubagentStop/Interrupt plain-text stdout is invalid, so print nothing. `async: true` guarantees it cannot block. Default timeout 600 s, so set `timeout = 3`-5.

### 5. Resume
`codex resume [SESSION_ID] [PROMPT]`: SESSION_ID is "Session id (UUID) or session name. UUIDs take precedence" (verified in local `codex resume --help`). Also `--last`, `--all`. Hook `session_id` is the thread UUID (legacy notify calls the same value `thread_id`), so `codex resume <session_id>` should work directly [the equality session_id==thread_id is from source reading, fairly confident]. Non-interactive: `codex exec resume`.

### 6. notify (legacy) vs hooks
`notify = ["prog","arg"]` in config.toml fires only on AfterAgent as `agent-turn-complete`, JSON appended as the LAST ARGV arg (not stdin): {type, thread_id, turn_id, cwd, client, input_messages, last_assistant_message}; stdio nulled. https://github.com/openai/codex/blob/main/codex-rs/hooks/src/legacy_notify.rs has "TODO: Remove ... when legacy notify support is removed". Use hooks in 0.162: they cover start/prompt/approval/stop/end with stdin JSON. Notify's only advantage is no trust review step; fallback only.

## GEMINI (local 0.63.0)

### 1. Config
- Docs: https://geminicli.com/docs/hooks/ ; reference https://geminicli.com/docs/hooks/reference/ ; repo docs/hooks/index.md.
- User file: `~/.gemini/settings.json`. Relocation: `GEMINI_CLI_HOME` replaces the home dir (packages/core/src/utils/paths.ts `homedir()`), so `$GEMINI_CLI_HOME/.gemini/settings.json` [path composition inferred]. Project: `<proj>/.gemini/settings.json`. System: /etc/gemini-cli/settings.json (Linux), /Library/Application Support/GeminiCli/settings.json (macOS). Plus extensions.
- Schema (docs):
```json
{
  "hooks": {
    "BeforeTool": [
      { "matcher": "write_file|replace",
        "hooks": [ { "name": "security-check", "type": "command", "command": "$GEMINI_PROJECT_DIR/.gemini/hooks/security.sh", "timeout": 5000 } ] }
    ]
  }
}
```
  Multi-event: add keys SessionStart, BeforeAgent, Notification, AfterAgent, SessionEnd each `[{ "hooks":[{ "type":"command","command":"..." }] }]` (matcher optional; "*" or "" = all). Fields: type (only "command"), command, name, timeout (MILLISECONDS, default 60000), description; source also shows an `env` object (hookRunner.ts).
- Matchers: regex for BeforeTool/AfterTool; exact string for lifecycle events (e.g. "startup").
- Merge: layers are MERGED (docs: precedence project > user > system > extensions, all merged; both run). Project hooks are fingerprinted with a warning when new/changed, and disabled if the folder is untrusted (hookRegistry.ts). User-level hooks: no trust prompt.
- Flags: `hooksConfig.enabled` (default true, restart required; packages/cli/src/config/settingsSchema.ts), `hooksConfig.disabled` (array of hook command names), `hooksConfig.notifications`. The older `tools.enableHooks` gate is not in current schema/docs [UNCERTAIN which version flipped it]. Slash commands: /hooks panel|enable-all|disable-all|enable <name>|disable <name>.
- Version: hooks shipped ~v0.26 (early 2026) behind a flag [UNCERTAIN]; 0.63.0 is later and current schema defaults enabled, so 0.63.0 should qualify [fairly confident].

### 2. Events and stdin (https://geminicli.com/docs/hooks/reference/)
Events: SessionStart, SessionEnd, BeforeAgent, AfterAgent, BeforeModel, AfterModel, BeforeToolSelection, BeforeTool, AfterTool, PreCompress, Notification.
Base stdin: `session_id`, `transcript_path`, `cwd`, `hook_event_name`, `timestamp` (ISO 8601).
- SessionStart: source = startup | resume | clear
- SessionEnd: reason = exit | clear | logout | prompt_input_exit | other
- Notification: notification_type = "ToolPermission" (only type; fired from packages/core/src/scheduler/confirmation.ts when a tool awaits confirmation), message, details {object}
- BeforeAgent: prompt
- AfterAgent: prompt, prompt_response, stop_hook_active
- BeforeTool/AfterTool: tool_name, tool_input (+tool_response); not re-verified.
Mapping: started = SessionStart; working = BeforeAgent; waiting for permission = Notification (ToolPermission); turn finished = AfterAgent; ended = SessionEnd. SessionEnd probably does not fire on kill -9/crash [UNCERTAIN], so add a liveness check.

### 3. Environment / parent
Docs: "sanitized environment". Source (packages/core/src/hooks/hookRunner.ts ~L348): env = sanitizeEnvironment(process.env, cfg) + GEMINI_PROJECT_DIR, GEMINI_PLANS_DIR, GEMINI_CWD, GEMINI_SESSION_ID, CLAUDE_PROJECT_DIR (+ hook.env). sanitizeEnvironment returns the full env unchanged unless `security.environmentVariableRedaction.enabled` (default false per docs/reference/configuration.md) or GitHub Actions mode (GITHUB_SHA / SURFACE=Github). When redaction is on, names matching TOKEN|SECRET|PASSWORD|PASSWD|KEY|AUTH|CREDENTIAL|CREDS|PRIVATE|CERT are dropped. So by default a launch-time env var is visible; avoid names with KEY/TOKEN/AUTH etc. `GEMINI_SESSION_ID` is also given directly.
Spawn: `spawn(shellExecutable, [...argsPrefix, command], {stdio: pipe, shell: false})` from the Node process, not detached. Parent is the shell, which may exec the command; getppid() == gemini only if the shell execs [UNCERTAIN]. Prefer env var / session id.

### 4. No-op
Exit 0 with empty stdout (or `{}`). If stdout has anything but a single JSON object, parse fails and the CLI defaults to allow. Logging goes to stderr. Exit 2 = block (never use). Other nonzero = non-fatal warning, continues. Hooks run SYNCHRONOUSLY in the agent loop (agent waits for all matching hooks), default timeout 60000 ms, so set `"timeout": 2000` and make the hook hand off and exit immediately. Avoid `continue:false`.

### 5. Resume
`gemini --resume <x>` / `-r`: "latest", an index number, or a session UUID (sessionUtils.ts message: "use --resume {number}, --resume {uuid}, or --resume latest"; local --help says 'Use "latest" for most recent or index number'). Sessions are per project (cwd-scoped), so run it in the session's cwd. Hook `session_id` equals the stored `sessionId` that resume matches, so it should be accepted directly when cwd matches. Other flags (local --help): `--session-id <uuid>` (start new session with given UUID), `--list-sessions`, `--session-file`.

## Practical recommendations
- Codex: write ~/.codex/hooks.json (or $CODEX_HOME), tell user to approve once in `/hooks`; hook reads stdin JSON, sends to daemon, prints nothing, exits 0; timeout 3, maybe async=true.
- Gemini: merge into ~/.gemini/settings.json (or $GEMINI_CLI_HOME/.gemini/), preserve existing keys; hook prints nothing/`{}`, exits 0, timeout ~2000 ms.
- Identify agent via a launch-time env var (visible in both by default) or session_id; do not rely on getppid().
