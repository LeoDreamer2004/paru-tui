# Native TUI development

## 当前状态

此 fork 的开发起点为 `9ac3578`。已实现首版原生 TUI、目录查询、设置与后端工作进程。使用方式和已知边界见 [TUI.md](TUI.md)。

开发分支：`tui/frontend`。

- `origin`：https://github.com/LeoDreamer2004/paru-tui
- `upstream`：https://github.com/Morganamilo/paru

目标是提供自己的更新计划、审阅、选择和执行界面，同时持续合并 paru 的上游改动。

## 设计边界

1. 后端沿用 paru 的依赖解析、构建顺序和配置语义，新增交互接口。
2. TUI 使用有类型的数据与后端通信，不通过解析提示文本或向 stdin 填入答案驱动更新。
3. 原始构建日志作为详情展示，不作为应用状态和用户决策的来源。
4. 实际授权、PKGBUILD 审阅和事务确认都有明确的交互位置；不能通过统一添加 `--noconfirm` 替代接口设计。
5. 更新查询与实际事务计划区分命名。界面不能把刷新前的候选列表当作最终安装计划。
6. 上游 CLI 保留为接口的默认实现，TUI 作为独立入口。尽量集中上游修改，避免将布局代码放入解析和安装逻辑。

## 源码切入点

| 文件 | 当前职责 | 需要抽象的部分 |
| --- | --- | --- |
| `src/lib.rs` | `run(args)` 初始化配置并分派操作 | 注入前端 / 后端会话，保留默认 CLI 入口 |
| `src/upgrade.rs` | 获取、打印更新并选择排除项 | 有来源和版本信息的更新数据及选择请求 |
| `src/install.rs` | 计划检查、审阅、构建和安装编排 | 最终计划、阶段事件、确认和结果 |
| `src/util.rs` | `ask`、`input`、提供者输入 | 问答协议；取消不应等价于接受默认值 |
| `src/resolver.rs` | 依赖提供者等交互 | 带候选数据的选择请求 |
| `src/exec.rs` | pacman、makepkg 等进程执行 | 日志、退出结果、取消和权限边界 |

特别注意 `install.rs` 中纯仓库目标直接运行 pacman 的路径，以及 pacman 自身的确认和进度。仅替换 paru 的 `ask` 无法覆盖整个更新流程。

## 已实现的交互模型

目录线程传递更新候选、查询结果和错误。工作进程通过 Unix socket 传递确认、输入、提供者选择、审阅、通知和构建阶段消息。原生 libalpm 事务工作进程提供 prepare 后的完整计划，在用户确认期间持锁。sudo askpass 使用独立连接，响应路由回对应请求。PTY 仅展示 makepkg 的构建输出，不承担控制协议。

当前工作进程同步串行发起问答，通道互斥保证只有一个待响应请求；取消是独立结果，断连和无效消息失败退出。若以后并发发起请求，必须增加请求 ID 和响应关联。目录搜索已有代次 ID，丢弃过期结果。

实际执行放在同一可执行文件的独立工作进程内，直接调用 fork 的 `crate::run`，通过专用 IPC 通道交换结构化消息。进程隔离便于处理上游信号逻辑、子进程输出及权限边界；TUI 本身无需以 root 运行。协议与界面布局独立，先验证接口再确定最终传输格式。

makepkg 和编译器的输出显示在 更新页底部的构建日志区域。F8 兼容模式已删除，审阅通过原生弹窗完成。

## 实现与后续工作

- `tui/transaction.rs` 是 libalpm 事务边界，保留原始 pacman 配置中的信任、仓库和忽略策略；不支持的选项明确报错。
- `tui/bridge.rs` / `session.rs` 管理结构化请求、密码通道和构建开始/结束握手。
- `install.rs` 的审阅分支不再调用外部 pager；提供者、包组和 PGP 导入也有原生交互。
- `tui/tree.rs` 缓存本地必需依赖图，按强连通分量处理无根循环组；UI 只展开用户打开的路径并复用列表渲染、详情和删除。
- `tui/devel.rs` 读取上游 devel 跟踪数据，复用 `devel.rs` 的远端提交查询，并由 `settings.rs` 为每个 Git 请求设置代理；扫描不推进提交基准。
- 后续扩展原生文件编辑、多构建日志归档和更多 CLI 参数。当前能力边界见使用说明。

旧 ParuView 原型已删除；可复用的 PTY 实现和终端测试方法已经迁入此 fork。此 fork 保持上游 GPL-3.0 许可信息。

## 跟随上游

将自己的功能提交成正常的 commit。同步前保证工作区修改已妥善保存，然后在开发分支执行：

```sh
git fetch upstream
git merge upstream/master
cargo fmt --check
cargo build --features tui --bin paru-tui
cargo test --features mock,tui --lib
python3 tests/tui_smoke.py target/debug/paru-tui
cargo test --features mock
```

合并后还需运行新增的前端协议和交互测试。上述命令不是自动发布流程；冲突和行为差异必须检查。`autostash` 仅处理尚未提交的工作区修改，不能代替分支维护和兼容性验证。

固定经过验证的上游提交。系统安装的 paru 更新不会自动更新此 fork 的后端代码。
