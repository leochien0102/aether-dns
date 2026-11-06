# aether-dns 任务卡：执行约定

> **项目与范围**：本文件的任务卡全部属于 **aether-dns** 仓库（`/workspace/openwrt/aether-dns`）。
> 执行时只在本卡分支/工作树中读写；**不准修改 aether-proxy 主检出、`archive/`、`openwrt-actions/`
> 或 `/workspace/openwrt` 下的任何其他仓库**（含其构建目录）。任务卡要求「参考」其他仓库时，
> 只读不写。设计文档、M0 报告、spike 证据都在本仓库内，执行者只能依靠仓库内容开工。
>
> 状态：**APPROVED（2026-11-04）**。所有者已确认 D1–D6，M1 卡组可分发；先派 T1.0/T1.1，
> 合并后再并行 T1.2/T1.3/T1.6/T1.7。

## 1. 角色

- **所有者**：定目标/范围，批准 `IMPROVEMENTS.md` 条目，确认里程碑 go/no-go。
- **设计负责人**（本会话）：拆任务、写任务卡与入口提示、复审交回、合并建议；不替执行者写卡内代码。
- **执行者**：只做卡内范围；有疑问**停下询问**，不自行扩大；交回即停。

## 2. 开工

**T1.0、T1.1 在 `main`/主检出执行**（此时还没有 worktree 脚本）；T1.1 完成后，后续卡按下面流程：
```bash
bash /workspace/openwrt/aether-dns/tests/start-task.sh <卡号>
```

脚本会：
- 从任务卡「**分支**」一行读出分支名；
- 基于本地 `main` 在 `/workspace/openwrt/aether-dns-wt/<卡号>` 建 worktree（已存在就复用）；
- 打印工作目录；主检出有未提交改动时拒绝创建。

把下面这段原样交给执行 agent（替换 `<卡号>`）：

```
任务：完成 aether-dns 仓库的任务卡 <卡号>（Rust 行为等价重写 chinadns-ng，当前里程碑 M1）。

卡片版本：<main commit>（发卡时填写；本卡以该提交的 docs/tasks/m1.md 为准）。
本卡只有一个目标，就是任务卡的「单一目标」；本卡只有一个完成标准，就是任务卡的「验收」。
不要用卡外的改进、顺手修复或额外测试替代验收；做不到就停，不扩成第二张卡的内容。

第一步：运行 bash /workspace/openwrt/aether-dns/tests/start-task.sh <卡号>
它会建好专用 git worktree 并打印工作目录。之后所有**写入与构建/测试命令**都只在那个目录里进行；
如果任务卡「分支」写的是 main（T1.0/T1.1 的例外），直接在主检出工作，不运行这个脚本。
不要修改 /workspace/openwrt/aether-dns（主检出），也不要碰其他卡的 worktree、archive 或 /workspace/openwrt 下的其他仓库。
任务卡必读的 aether-proxy 文件（renderer、fixtures、component_version.uc/daemon.uc）**可以只读**；
不要写入 aether-proxy，也不要在其中运行会写文件的命令。

开工前在该目录中完整阅读：
1. docs/tasks/README.md「执行约定」（本文件）
2. PLAN.md 的 M1 与 P0 范围、M0-REPORT.md 中与本卡相关的章节
3. IMPROVEMENTS.md 中已批准的条目（尤其是 IMP-004/005）
4. 任务卡本身（docs/tasks/m1.md 中对应小节）

开工第一步后，先核对卡片版本；非 0 就停，不要按旧卡实现：
  git -C <worktree> diff --quiet main -- docs/tasks PLAN.md
非 0 表示 worktree 里的卡/文档不是当前 main 版本：向设计负责人要「卡差量」或重建 worktree。

只做任务卡范围内的事。设计文档没覆盖、或卡上写了「停点」的情况，不要自行决定：
按最保守方式实现并标注 TODO(design)，写进报告，然后**停下询问**。
报告开头先照抄本卡的「单一目标」和「验收」清单；实现过程中不要改这两项。
不准实现 P1/P2 功能，不准引入卡外的架构/依赖，不准用 nft CLI 实现 nftset，不准手写完整 DNS parser。

完成后：cargo fmt --all -- --check、cargo clippy --all-targets -- -D warnings、cargo test --all 全过；
运行 bash tests/review.sh，FAIL 必须为 0；按约定写 reports/<卡号>.md 并提交到本卡分支。
不要 git push，提交信息不要带任何 AI 署名尾注。交回即停，不再改动分支和工作区。
```

### 2.1 质量门环境

`tests/review.sh` 需要 `cargo`、`rustfmt`、`clippy`。本 WSL 已安装 apt `rustfmt` / `rust-clippy`
（Rust 1.93.1），干净 PATH 下可直接运行；执行者不需要 `sudo`、不需要手动改 PATH。
新环境缺少组件时，停在报告里说明，不要自行安装。

### 2.2 分支/worktree 策略（2026-11-04 决定）

- 从 **T1.2 起**，每张实现/验收卡一个**分支 + 一个 worktree**，从 `main` 建；不复用其他卡的分支或 worktree。
- 例外：T1.0/T1.1（bootstrap，已在 `main` 完成）、设计负责人的 docs/review 提交。
- 并行上限 **3 张**；强依赖或改同一文件的卡串行（T1.4→T1.5、T1.6→T1.7）。
- 复审通过后由设计负责人 `git merge --no-ff --no-commit`，删除 `reports/<card>.md` 后提交 merge；
  再删除 worktree 与分支，并在 `main` 上跑一次 `tests/review.sh`。
- 复审不通过：执行者在**同一分支**修，不新开卡、不 merge。
- 理由：`main` 始终可构建；每卡 diff 可审查/可回滚；多个 agent 可并行。worktree 开销在本机可忽略
  （12 核、894 GB 空闲），远小于返工与“main 被半成品污染”的成本。

## 3. 禁止事项

- 不准 `git push`、切换别的分支、修改主检出或其他 worktree/仓库。
- **只读例外**：任务卡「必读」列出的 aether-proxy 文件（renderer、22 份 fixtures、
  `component_version.uc`、`daemon.uc` 等）可以用 `cat`/`grep`/`git show` 只读；
  不准写入、不准在其目录内运行会写文件的命令。**不需要把文件复制进 worktree。**
- 不准 `sudo`、安装系统软件包、运行容器，除非任务卡明确允许（M1 卡不允许）。
- **不准擅自重启真机/路由器**：`reboot`、`poweroff`、固件重启、看门狗复位、断电重启都必须先获得所有者明确授权，并说明维护窗口与回退方式；怀疑启动故障时先停下询问。
- 不准把 `main` merge/rebase 进本卡分支；冲突留给复审。
- 不准修改 `docs/`（任务卡/设计文档）——发现矛盾写进报告，由设计负责人改。
- 不准复制归档项目 `archive/aether-dns-2026-11-04/` 的代码；M0 spike 可参考，但要重写并加测试。
- 不准“顺手”实现 P1：DoT、hosts、cache-db、完整 6 组、IPv6 代理、aether-proxy 的任何功能。
- 不准把 spike 代码原样搬进 `src/` 当生产代码；M1 的模块必须有自己的测试与错误处理。
- 需要下载依赖/工具时，只能用 cargo 依赖或仓库外临时目录，并在报告中写明。

## 4. 完成标准

1. `bash tests/review.sh` 的 FAIL 为 0（fmt、clippy、test；M1 后期含 harness）。
2. 任务卡「验收」中能在本地验证的条目都已验证，命令与关键输出写进报告；不能验证的写原因。
3. 工作区干净，所有改动已提交到本卡分支；没有占位符/TODO 未说明。
4. 新增/修改代码有测试覆盖；修 bug 时，去掉修复后至少一条断言会失败。
5. 没有卡外改动；`git diff main...HEAD --stat` 与卡的「允许改动」一致。
6. 交回后停止改动。

提交信息用 `feat(<范围>): …`、`test(…)`、`docs(…)`、`chore(…)` 这类前缀；**不准**带
`Co-authored-by` 或任何 AI 署名尾注。

## 5. 报告格式

在分支中提交 `reports/<卡号>.md`（例如 `reports/T1.2.md`）。复审合并时移出 `main`。

```markdown
# <卡号> 完成报告

## 改动
- 新增/修改的文件，每个一句话说明

## 验收
- 任务卡每条验收：怎么验证的（命令）、结果（关键输出）；未验证的写原因

## 测试
- 新增/修改的测试与断言；覆盖了卡的哪些要求
- 变异测试（如做了）：故意破坏什么，哪条断言失败

## 偏差与待定
- 与设计/任务卡不一致的地方及理由
- 代码中的 TODO(design) 列表
- 发现的文档矛盾或需要所有者决定的问题（**停点**）

## 下载与环境
- cargo 新增依赖及理由；其他下载（没有就写「无」）
```

## 6. 复审（设计负责人）

0. 先核对卡本身：只有**一个目标**、验收是**可执行命令/可判定结果**、有「不做什么」和「停点」；
   不满足就退回重写，不派工、不合并。
1. 读报告 + 卡，核对「允许改动」与 `git diff main...HEAD --stat`；有卡外改动直接退回。
2. 跑 `bash tests/review.sh`，核对报告输出；逐条核对验收命令与证据。
3. 读关键代码：范围、错误处理、停止条件、测试是否真的能失败（必要时做变异测试）。
4. 检查没有 P1 功能、没有 aether-proxy 改动、没有把 M0 spike 原样搬入。
5. 通过则按 §2.2 合并：`git merge --no-ff --no-commit <branch>` → `git rm reports/<card>.md` →
   提交 merge → `git worktree remove <path>` → `git branch -d <branch>` → 在 `main` 跑 `tests/review.sh`。
   不通过则写清退回原因，执行者只改退回项，不扩大。
6. 里程碑验收由设计负责人跑 harness/基线与真机（M3），不只依赖执行者报告。

## 7. 卡片类型与硬性规则

- **实现卡（T）**：产出代码与测试，验收是命令及预期输出。
- **探针卡（S）**：验证假设，产出写回文档；原型放 `spikes/`，不写生产代码。

每张卡必须满足：

1. **只有一个目标**，写成一句话；出现“并/同时/顺带/另外”就是拆卡信号。
2. **验收必须是可执行命令 + 可判定结果**（退出码、输出行、断言名）；“报告里说明”不能作为唯一验收。
3. 必须有：目标、分支、必读、允许改动、做什么、**不做什么**、**停点**、验收、依赖。
4. 卡内发现的新问题：写进报告并停；**不扩卡**，由设计负责人另开卡。
5. 没有「不做什么」和「停点」的卡不分发；验收不可执行的卡退回重写。

## 8. M1 决策（所有者已确认 2026-11-04）

| ID | 决策 | 建议 | 状态 |
|---|---|---|---|
| D1 | M1 是否启动；IMP-004/005/006 是否批准 | 批准并启动 M1 | 已确认（2026-11-04） |
| D2 | 仓库/分支流程：`git init` + `tests/start-task.sh` + 本地 worktree；暂不 push/CI | 采用，和 aether-proxy 同款 | 已确认（2026-11-04） |
| D3 | M1 运行时范围：纯转发（所有查询 → 第一个 `trust-dns`），组/列表/tag/nftset/cache 全留 M2 | 采用；M1 只证明“配置能读、端口能听、报文能转发” | 已确认（2026-11-04） |
| D4 | `-V/--version` 版本方案：aether-proxy 取首行第二字段与 `2025.08.09` 比较 | 用日期版本 `2026.11.04`（诚实、可过闸门）；不冒充 chinadns-ng | 已确认（2026-11-04） |
| D5 | 异步运行时：Tokio current-thread（PLAN 已写） | 采用；M1 不引入多线程/worker | 已确认（2026-11-04） |
| D6 | M1 验收是否复用 M0 Python harness | 不复用：T1.8/T1.9 用独立脚本 + 本地假上游，不依赖 netns/root；M2 再复用 harness | 已确认（2026-11-04） |

以上 D1–D6 已确认；`m1.md` 可分发。后续若改范围，必须先改这些决策并重新确认，不直接改卡。

---

## 9. 发卡前检查（强制，设计负责人）

每张卡进入入口提示前，逐项打勾；任何一项不过，不发卡：

1. **单一目标 + 可执行验收**：命令、退出码、输出/断言；Rust 测试命令写 `cargo test --test <target>`，
   不用 `cargo test <名字>` 猜过滤。
2. **允许改动完整**：实现会碰的每个文件，包括 `Cargo.toml`/`Cargo.lock`、模块声明、测试数据、报告；
   在空 worktree 里预演 `git status`，确认不会出现“卡外文件”。
3. **外部引用明确**：每个仓库外文件写「只读」还是「提供副本」，入口提示重复；禁止“不要碰”这种
   会被理解成“不能读”的措辞。
4. **文档一致**：与 CHARTER/PLAN/IMPROVEMENTS/M0-REPORT 对照；冲突先改文档，不改卡绕开。
5. **卡片冻结**：`main` 干净并提交后才创建 worktree；入口提示写明 `卡片版本 = <main commit>`。
6. **空跑**：用 `start-task.sh` 建临时 worktree，按验收命令跑一遍（允许预期失败），确认路径/工具/端口；
   跑完删除。
7. **入口提示复审**：目标、验收、只读授权、允许改动、停点五件事齐全；不写卡外要求。
8. **发卡后文档变化**：必须同步/重建 worktree 或发「卡差量」，不能只口头通知。执行者开工后先跑
   `git -C <wt> diff --quiet main -- docs/tasks PLAN.md`；非 0 就停，
   要卡差量，不按旧卡实现。
9. **跨仓库测试数据**：测试必须在 `tests/data/` 自带副本或用环境变量指向；不得硬编码其他仓库的
   绝对路径（T1.2 已有一处，已记录；跨机器/CI 前补副本）。
10. **行为变更影响面**：卡改变可观察行为时，必须列出**所有**既有 acceptance/test 的 fixture 假设，
    逐条写“继续通过 / 需在本卡更新 / 作废”，并把需更新的文件加入允许改动。
    禁止只写“X 不回归”而不核对具体 fixture。
11. **发卡前跑全验收**：在当前 main 或临时 worktree 跑 `tests/review.sh`、`tests/acceptance_startup.sh`、
    `tests/acceptance_forward.sh`、`tests/nftset_acceptance.sh`；若新行为会让某个验收失败，
    卡内必须包含迁移，否则不发。
12. **迁移测试不算扩范围**：为保持验收可执行而更新既有测试/脚本，必须在卡中显式授权；
    只改输入/期望/注释，不借机改生产语义。
13. **卡差量复盘**：每次停点确认后，在 `docs/tasks/README.md` 或卡里补一条“为什么发卡时没发现”；
    同类原因第二次出现，先改流程再发下一卡。
14. **影响表必须有 grep 证据**：行为变更卡的影响表不能凭印象写“无过滤/无某指令”。发卡前对
    `tests/` 逐个 grep 相关配置字符串（至少 `no-ipv6`、`filter-qtype`、`default-tag`、
    `add-taggfw-ip`、`group-ipset`、`cache`），把命中文件与处理方式写进卡。
15. **有状态卡必须核对重复 key**：引入 cache/verdict/连接复用等状态时，必须静态或实际确认
    既有验收脚本里有没有同名 question 重复发送；不能写“qname 各不相同”。至少检查
    `fake_upstream.py` 的 qname/qtype/transport 循环和 `tests/*.rs` 的重复 query。

## 10. 当前卡组

| 文件 | 里程碑 | 状态 |
|---|---|---|
| [m1.md](m1.md) | M1 骨架：T1.0–T1.9（构建、worktree、配置、wire、UDP/TCP 转发、CLI、信号、两项验收） | **APPROVED**；全部完成 |
| [m2.md](m2.md) | M2 P0 行为：列表/tag、上游组、nftset、路由、IP 测试、过滤、cache、baseline 差分 | **完成**；12/12 卡，baseline 29/29 + 13/13 |
| [m3.md](m3.md) | M3 真机集成：安装/切换/回滚/冷重启、模式矩阵、T1/T2/T3、故障演练、观察 | **进行中**；M3-01 完成，M3-02/03 待窗口 |

## 11. M2 执行约定（2026-11-05）

- 范围锁在 PLAN §2 的 P0；P1（hosts、DoT、cache-db、完整 CLI、chnlist-first、6 组）不做。
- nftset 测试用 **unprivileged `unshare -Urn` + `nft` 建表**，不需要 sudo/root 宿主；
  生产实现必须 netlink，不能用 `nft` CLI。
- M0 `baseline.json` 的 9 场景/29 查询与 13 条 sanity 是 M2 reviewer 验收门；执行者用 fake upstream
  做卡内测试，不要求在本机跑 root harness。
- cache 只做内存（无 DB）；TTL 原地递减，不重编码。
- 第一波 M2-01/02/03 并行；M2-04 等前两张列表/上游卡合并后再拆。
