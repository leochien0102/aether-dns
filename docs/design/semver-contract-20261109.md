# aether-dns × aether-proxy 版本契约（草案，2026-11-09）

> 状态：**DNS 侧已落地 `1.0.0`（M4-03，仓库代码/测试/文档），尚未打包/部署；proxy 侧未拆版本闸门**，拆分前不得将该版本部署到以 aether-dns 为 DNS 引擎的设备。
> 本文件是跨项目接口约定，**不是实现授权**：不构成构建、发版、安装或设备操作许可。
> 生效前提：两项目各自冻结卡并按 §6 顺序落地；落地后由负责方更新本文状态行。
> DNS 侧权威状态见[独立交接](../handoffs/session-handoff-20261109.md)；proxy 侧见其交接与 [M4 卡](../../../aether-proxy/docs/tasks/m4.md)（L5）。
> 本文所有 proxy 行为均于 2026-11-09 只读核实自代码，不是转述文档。

## 1. 已确认决策（所有者，2026-11-09）

| 项 | 决定 |
|---|---|
| aether-dns 首个独立版本 | **`1.0.0`**（纯 `x.y.z`） |
| proxy 兼容范围 | **只设下限 `>= 1.0.0`**；上限只写在文档，不进代码 |
| ipk 版本处理 | 设备现包 `2026.11.04-1` **从未正式发布**，故无需 force-downgrade 论证；改为 `1.0.0-1` 后可手动覆盖安装或卸载重装 |
| 与 proxy 的关系 | 不与其同号；proxy 只能按引擎声明最低版本，不得假设两者版本一致 |

## 2. 数值选择依据

- 已装基线（源码 `325b614`、包 `2026.11.04-1`）已覆盖 proxy 当前 renderer 的完整 P0 消费面（P0-01…P0-11），并含 M4-01 缓存刷新，具备 1.x 的“可替换”含义。
- 与 proxy `0.6.3-r7` 明显不同号。
- 首段 `1` 与 `26.3.27`（xray）、`2025.08.09`（chinadns-ng）不会互相误比——拆分后各引擎各比各的。

## 3. proxy 现行机制的硬事实（2026-11-09 只读核实）

| 事实 | 位置 |
|---|---|
| 只取 `-V` 首行第二个空白分隔字段 | `aether-proxy/src/lib/component_version.uc:42-48` |
| 版本串白名单 `^v?([0-9]+(\.[0-9]+)+)(-r[0-9]+)?(\+[0-9A-Za-z._-]+)?$` | 同文件 `:10` |
| 必须 ≥2 段纯数字：`1` 不合格；`1.0.0-rc1` → `null` | 同上 |
| 比较按数字段、零填充、忽略 `-rN` 与 `+meta` | 同文件 `:16-40` |
| 启动闸门**致命**：`version_cmp < 0` → `ok:false, code:1, step:'component_version'` | `src/lib/daemon.uc:567-584` |
| 闸门按**组件名**取最低值 | `daemon.uc:570`、`cmd/check.uc:172`、`const.uc:30-31` |
| `check` 中“过旧”只是 warning | `cmd/check.uc:161-176` |
| 设备上逻辑键 `chinadns-ng` 当前指向 `/usr/bin/aether-dns` | proxy `docs/tasks/m4.md:22-31` |

推论（决定顺序的根源）：

1. `version_cmp('1.0.0','2025.08.09') = -1`（首段 `1 < 2025`）。
2. 所以**先**把 DNS 改成 `1.0.0`、而 proxy 仍是现行按名闸门时，**daemon 会拒绝启动**，不是告警。
3. 反向安全：`version_cmp('2026.11.04','1.0.0') = +1`，proxy 先拆闸门不影响现网日期版本。

## 4. 规则（两侧实现必须遵守）

- **R1 格式**：DNS `-V` 第二字段必须是 `x.y.z`（≥2 段、纯数字）。本轮 `1.0.0`。
- **R2 禁止预发布后缀**：不得出现 `-rc`/`-beta` 等；proxy 解析为 `null`（未知，不致命但无法验证）。
- **R3 未知值语义**：`-V` 输出异常，或 engine 无法判定（basename 既非 `aether-dns` 也非 `chinadns-ng`）→ 保持“信息性、不致命”，与现有“未知版本信息性”一致。
- **R4 最低值按引擎**：`aether-dns` → `1.0.0`；`chinadns-ng` → 保持 `2025.08.09`。
- **R5 上限不进代码**：proxy 不对 `2.0.0` 做硬拒绝；将来若需要，另立跨项目决策。
- **R6 契约形状不变**：`COMPONENT_TOO_OLD` / `COMPONENT_MISSING` 的字段形状与 `component` id（取 M4-02 阶段 2b 的终值）不变，只有 `min` 的来源改为按引擎。

## 5. 各自改动清单

### DNS 侧（卡 A，本仓库）

`src/main.rs:42`（banner）、`tests/cli.rs:34`（banner 断言）与 `:36`（最低版本断言改为 `>= 1.0.0`）、`Cargo.toml:3` 与 `Cargo.lock:7`、`README.md` 当前状态段、`docs/handoffs/session-handoff-20261109.md` 版本表、`docs/tasks/m4.md`、`reports/M4-03.md`。

明确不改：查询行为、配置、缓存；ipk 配方（在 `openwrt-actions`，另一仓库）；历史报告与 PLAN 里程碑段里的 `2026.11.04`。

### proxy 侧（卡 B，proxy 仓库）

`const.uc` 新增 `AETHER_DNS_MIN_VERSION = '1.0.0'`（保留 `CHINADNS_MIN_VERSION = '2025.08.09'`）；`daemon.uc:570` 与 `cmd/check.uc:172` 改为按 `engine` 选常量；`tests/daemon/version-gate.py:17-18` 的 `MINIMUM`/`BANNERS` 扩引擎并补场景；`docs/04`、`docs/01` 契约文字。

`component_version.uc`（解析与比较）**不动**。

依赖提示：M4-02 阶段 2b（`M4-02b2`）的 S2 断言 `COMPONENT_TOO_OLD` 的 `min` 仍为 `2025.08.09`；卡 B 必须**显式迁移该断言**，不得顺手改。

## 6. 上线顺序（强制）

1. proxy 拆闸门（卡 B）并发布。
2. aether-dns 出 `1.0.0` 包（卡 A）。
3. 设备安装（需所有者单独授权）。

禁止：在 1 未落地时把 `1.0.0` 装到以 aether-dns 为 DNS 引擎的设备——会拒启。若必须同窗，则同一维护窗口内先 proxy 后 DNS。

## 7. 验证矩阵（卡 B 必须覆盖）

| 场景 | 期望 |
|---|---|
| aether-dns `2026.11.04` | 通过（迁移期兼容） |
| aether-dns `1.0.0` | 通过（等于下限） |
| aether-dns `0.9.9` | 启动拒绝：`code 1`、`step component_version`、`COMPONENT_TOO_OLD.min == '1.0.0'` |
| 真 chinadns-ng `2025.08.09` | 通过 |
| 真 chinadns-ng `2025.08.08` | 启动拒绝，`COMPONENT_TOO_OLD.min == '2025.08.09'` |
| `-V` 输出为空/畸形 | 非致命：仅诊断信息，daemon 继续 |
| `check` 遇到以上“过旧” | 仍只 warning，不改变退出码 |

卡 A 的验证：`cargo test --test cli` 的 banner 断言为 `aether-dns 1.0.0`；变异（改成 `0.9.9`）必须让 `>= 1.0.0` 断言变红。

## 8. 边界与未决

- `2.0.0` 及以后：因 R5 无上限，proxy 不会拒绝；是否需要大版本闸门另行决定。
- 真实 `chinadns-ng` 与 `aether-dns` 可共存（现 H28K 两者都装），因此闸门必须按 engine，而不是按路径或包名。
- opkg epoch 方案**未验证**，本约定不采用；包版本迁移走手动重装。
- 本约定不改变 DNS 行为，不构成 `IMPROVEMENTS.md` 或 `PARITY-MATRIX.md` 的 parity 变更登记。

## 9. 变更记录

- 2026-11-09 草案：所有者确认 `1.0.0`、仅下限、手动重装；两侧均未落地。proxy 侧待 M4-02 收尾后再开卡（避免与阶段 2b 的 `check.uc`/`daemon.uc`/`version-gate.py` 同行冲突）。
