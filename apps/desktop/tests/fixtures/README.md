# Renderer UI fixture

From the repository root:

```sh
node apps/desktop/tests/serve-ui-fixture.mjs
```

Open `http://127.0.0.1:1421/`. An optional positional port overrides `1421`.
The server listens only on `127.0.0.1` and fails if that port is occupied.
Stop with Ctrl+C.

This serves the real React renderer through Vite with a test-only script injected
before the renderer loads. All Tauri IPC is intercepted. Commands use in-memory
data, `.example.test` addresses and fake credentials. No native process, real
Codex configuration, app database, or remote API is used. Unknown IPC commands
fail explicitly. The fixture does not alter production entry points or Vite
configuration.

## Delayed template detail and cross-page controls

1. Open **指令提示词**. Its automatic template sync remains pending.
2. Click the edit button for **Fixture 内置模板**. The detail read also remains
   pending independently of the sync.
3. Open **供应商** while both requests are pending. **新增供应商**, editing a
   supplier, and enabling **Fixture Saved Provider** should remain usable when
   the template-detail loading state is correctly scoped to its page.
4. Use **Fixture：完成模板详情** after leaving the prompt page. Returning to
   **指令提示词** should not unexpectedly open an editor from that stale request.
5. **Fixture：完成模板同步** resolves the pending catalog sync separately.

Before the UI fix, step 2 uses the shared `loading` flag and step 3 shows disabled
supplier actions. The delayed detail is intentional even after backend reads are
made fast, so this fixture can test the UI against a slow IPC response.

The panel sits in the lower-left corner. Click **Fixture：所有命令均为内存模拟**
to collapse it when testing the sidebar. The appearance switch can also be
focused and toggled with the Space key.

The fixture controls are:

- **Fixture：完成模板同步**
- **Fixture：使模板同步失败**
- **Fixture：完成模板详情**
- **Fixture：使模板详情失败**

Each template control settles all currently pending requests of its kind.
Clicking it with no pending request has no effect. Reloading resets pending
requests and command counters, while provider and official-profile data survive
in this fixture origin's `sessionStorage` (`codexx.fixture.backend.v2`). Use
**Fixture：重置测试数据** for a clean starting model. The fixture also resets the
renderer's language, startup-wizard marker, remembered provider and prompt
categories in this fixture origin's localStorage.

## Multiple official logins

1. Reset the fixture. The default **OpenAI Official** profile has ID
   `openai-official` and fake OAuth identity `fixture-account-one`.
2. In **供应商**, select **新增供应商**, change **供应商类型** to
   **官方 Codex 登录**, name it **第二账号**, and leave authentication empty.
   Save it. The current supplier must stay unchanged; the new profile has
   `hasAuth: false` in the fixture diagnostics.
3. Enable **第二账号**. Diagnostics must show its new `activeOfficialProfileId`,
   with `currentOfficialAccount: null`. This models waiting for a Codex login.
4. Click **Fixture：登录第二个官方账号**. This changes only the live in-memory
   official authentication to `fixture-account-two`, modeling an external Codex
   sign-in. It does not immediately modify the saved profile snapshot.
5. Switch to **OpenAI Official**, then switch back to **第二账号**. The fake
   backend captures the previous live login before switching, so the diagnostics
   should alternate between `fixture-account-one` and `fixture-account-two`.
   Reload the browser and repeat to verify that the fixture model survives reload.
6. To test explicit capture, activate an official profile, use the fixture login
   button, open another official profile's editor, click **读取当前官方登录**, and
   save. Authentication is copied into that chosen profile without switching the
   current supplier.
7. Copy **OpenAI Official**. A named copy appears immediately in the list, with
   the same fake authentication and a distinct ID. No editor or confirmation
   dialog opens. Copy it again to check that names gain a numeric suffix. Open
   the copy's editor separately to rename it. Clearing its authentication and
   saving must leave the default profile unchanged.
8. Delete a named profile. The fixture rejects deletion of the default or the
   currently active profile, so the frontend must switch to the default first
   when deleting an active named profile.

The official-login fixture implements `list_official_profiles`,
`get_official_profile`, `save_official_profile`, `duplicate_official_profile`,
`switch_official_profile`, and `delete_official_profile`. Saving a new/inactive
profile does not switch; saving the active profile updates its live in-memory
configuration. `authJson: null` preserves a saved profile's authentication; for
the current official profile it captures the latest simulated live login.
`authJson: ""` explicitly clears authentication. Both default and named profiles
can be renamed. The additional
controls are:

- **Fixture：登录第二个官方账号** — enabled only while an official profile is active.
- **Fixture：重置测试数据** — removes the fixture session data and reloads.

For browser assertions, these actions are also exposed as
`window.__CODEX_X_FIXTURE__.loginSecondOfficialAccount()` and `.reset()`.
After editing `ui-runtime.js`, restart the fixture server: it reads the injected
script once at startup.

## Immediate copies and 1M context

1. Copy **Fixture CC Switch** before switching providers. The detected original
   is preserved and a separate copy appears. The original stays current. Repeat
   with **Fixture Saved Provider** and an official profile: every click creates
   a unique name and ID, without opening an editor or confirmation dialog.
2. Open either provider type's editor. Next to **config.toml**, enable
   **开启 1M 上下文窗口**. The draft gains top-level `model_context_window = 1000000`
   and, if absent, `model_auto_compact_token_limit = 900000`. Nothing is saved
   until the form's save button is clicked.
3. Edit the context value manually to `1_000_000` and then `256000`; the checkbox
   should follow the draft. An existing compaction value such as `850000` must
   survive enabling and disabling. Turning on and off within the same edit
   restores the previous custom context value.
4. Add an MCP table and test the toggle again. Those lines remain present. A
   `model_context_window` inside an MCP/profile table or a multiline string
   must not make the top-level checkbox checked. Invalid context-field types
   or an unfinished table header disable the checkbox and show an inline hint.
5. Save, reopen the editor, and check that the 1M settings persist in the fake
   profile. Resetting the whole fixture intentionally removes them.

`update_codex_context_window` is a **limited mock**, supporting the simple
line-based TOML used in these scenarios. It recognizes quoted context keys,
integer separators, comments, tables and basic multiline strings. It does not
validate arbitrary TOML and is not a substitute for the production `toml_edit`
parser. The Rust `context_config` tests establish parser and mutation behavior.
The same limitation applies to the fixture's provider form-to-TOML updates.

## Usage statistics

Open **设置 → 用量统计**. The fixture generates synthetic records relative to
today across 40 days and three models. Input includes cached input; output
includes reasoning; total is input plus output. The 7-day trend includes a day
with no usage. **今天**, **近 7 天**, **近 30 天**, **全部** and the model filter
actually aggregate different subsets. The daily and model sums must agree with
the total cards. The recent-session list is capped at 10 while totals include
all matching records. Rows show sample chat titles, not shortened session IDs.
These sample rows represent main conversations with their subagent usage already
included. Thread classification, recursive ownership, and replay deduplication
are covered by the Rust usage/SQLite tests, not simulated by this UI fixture.
Switch between **通用设置** and **用量统计** to check the shared fade-out/fade-in
transition. Returning to usage keeps the selected date range and model.

Use the following panel controls, then click **刷新用量** (or **重试**):

- **Fixture：用量示例数据** restores the normal sample.
- **Fixture：用量空数据** returns zero totals and no sessions/models.
- **Fixture：用量读取失败** rejects the read. A first read shows the error view;
  after a successful read, a failed refresh should keep the last result with a
  failure notice.
- **Fixture：用量部分数据** returns the sample with two synthetic skipped files,
  so the coverage notice is visible.

The selected mode appears in diagnostics as `usageMode`; it resets to `sample`
on reload. Tests may call `window.__CODEX_X_FIXTURE__.setUsageMode(mode)`.
`get_usage_statistics` accepts `configDir`, `range`, `model` and `forceRefresh`
with the production response shape from `src/usageTypes.ts`. Both refresh
paths return newly aggregated sample data; there is no filesystem scan or
real usage cache in this fixture.

## Official account badges and quota

Reset the fixture when migrating older session data so synthetic authentication
includes `one@example.test`. **Fixture：登录第二个官方账号** writes
`two@example.test`. Official summaries expose `email`, `hasOwnedAuth`, and
`canQueryQuota` from the selected profile's live authentication or its own saved
snapshot. An empty official profile shows the unsigned-in badge. No additional
profiles are added by default; use **新增供应商 → 官方 Codex 登录** to create one.

Use the compact **Fixture：额度模式** selector before clicking a profile's quota
button or the quota dialog's refresh action:

- **双窗口 Team**: 5-hour and weekly windows with 93% and 52% remaining.
- **单周 Pro**: one weekly window with 91% remaining.
- **Spark 双窗 + reserve 周**: the main windows plus Spark 5-hour/week and a
  separate reserve weekly allowance.
- **无主额度窗口**: no limits and an unknown plan.
- **未知窗口周期**: a 2-hour window plus an unknown duration/percentage/reset.
- **认证过期 401**: a sanitized sign-in-expired error.
- **延迟：完成后成功／失败**: keeps the request pending until
  **Fixture：完成额度请求** is clicked. The scenario is captured when the request
  starts, so changing the selector does not alter an already pending response.

Changing fixture mode does not trigger a request. Diagnostics expose `quotaMode`
and `pendingQuota`. Node assertions may use `setQuotaMode`, `completeQuota`, and
`failQuota` on `window.__CODEX_X_FIXTURE__`. Responses follow
`src/officialQuotaTypes.ts`; all accounts, limits and timestamps are synthetic,
and the fixture never contacts the ChatGPT endpoint.

## Provider model mappings

The fixture preserves `modelMappings` when saving and copying a supplier, and
implements `activate_saved_provider` with the production `providerId` argument.
Fetch models, expand **模型映射**, import the list, and edit the display name,
actual model ID or optional context window. Invalid IDs, duplicate rows and
invalid context sizes disable Save. Reopening a saved item retains its mappings;
enabling it shows the restart notice. Actual catalog files, outbound model IDs,
authentication and rollback are covered by the Rust and native Codex checks.

## Provider presets and retained usage results

The add-provider form exposes eight preset cards. Cards show only brand marks and names; switching among all presets, including official sign-in, keeps the same add-page transition key. The initial custom TOML includes a reasoning setting, a fixture MCP server and a project entry for inheritance checks. Selecting a vendor fills its
native Responses endpoint and model mappings. MiniMax region and MiMo plan
buttons change the endpoint and clear the synthetic key. Reopening a saved
preset retains its selected model and mappings.

For the usage-switching regression, load the initial seven-day result, then turn
on **Fixture：暂停用量响应**. Select Today, 30 days and All time in succession.
The existing summary and charts should remain visible with their original range
label and a small loading status. **Fixture：完成最新用量请求** resolves the last
selection first; **Fixture：完成旧用量请求** resolves one older request at a time.
Those older results must not replace the latest displayed range. Diagnostics
include `pendingUsage`; all data remains synthetic and in memory.

## Reset credits and plan badges

Official fixture logins expose a `planType` (the default login is Pro, and the
external second login is Pro Lite). Additional fake official logins can specify
`plan_type` in their synthetic auth JSON to inspect Plus, Team or other badges.
No account key or network request is involved in these metadata displays.

**Fixture：重置次数模式** controls the independent reset-count response: 3, 0,
failure, delayed success or delayed failure. To verify concurrency, select a
delayed quota and delayed reset-count response, open the quota dialog, and check
`pendingQuota` and `pendingResetCredits` are both 1 in the visible diagnostics.
**Fixture：完成重置次数请求** resolves only the count; it must render while quota
is still loading. **Fixture：完成额度请求** resolves the other request. Closing
and changing profiles invalidate both pending results. There is no redemption
operation in the fixture or this feature.

## Diagnostics and scope

The accessible **Fixture 命令记录** output (`#fixture-command-log`) displays JSON
with pending counts, invocation counts, provider switches (including official
profile IDs), saved provider IDs, official-profile summaries, fake account IDs,
and the ordered command names. Authentication tokens are omitted from this
diagnostic output. The same snapshot is available to browser assertions:

```js
window.__CODEX_X_FIXTURE__.snapshot()
```

Initially **Fixture CC Switch** is a detected current supplier, while
**Fixture Saved Provider** is the only saved entry. The fake backend preserves
the detected entry when switching away, allowing the renderer to show both
entries and switch back. This models the desired backend response; it does **not**
validate Rust persistence, credential resolution or database behavior. The same
limitation applies to official-profile capture, independence, and storage: these
responses are models for frontend verification. Use the Rust regression tests
for backend guarantees. All persisted fixture credentials are synthetic and
scoped to the local fixture origin and browser session.
