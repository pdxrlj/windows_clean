# 方向确认 · CleanSweep 界面重设计

## 展示了什么

| 文件 | 内容 |
|---|---|
| `thumbs.html` / `thumbs.png` | 六个候选方向的骨架缩略图（A 仪器面板 / B 瑞士网格 / C Linear / D Zed / E 磁贴盘点 / F 宋刻本） |
| `dir-1.html` / `dir-1.png` | **A · 仪器面板**（Teenage Engineering 路线）完整版 |
| `dir-2.html` / `dir-2.png` / `dir-2b.png` | **C · 三级明度 + 属性栏**（Linear 路线）完整版，含未选中态的机器体检卡 |
| `dir-3.html` / `dir-3.png` | **E · 磁贴盘点**（新粗野主义，色彩降级）完整版 |
| `directions.md` | 六张方向卡（逻辑来源、借了什么、主角素材、骨架、胆量花在哪、form 来自内容的哪里） |
| `spec.md` | 三方向共同输入（受众、核心信息、情感基调、尺寸、已知约束、可读性底线） |
| `refs/` | 现实参照截图（linear.png / zed.png / raycast.png） |

三版同一份内容、同一 1280×800 基准、都能真实点击（切页签、点行、再点取消选中）。

## 用户的选择

问题：三个方向选哪一个落地到 React（App.tsx + styles.css + 各 View）？

> 用户选中：**A · 仪器面板**
> （选项原文：「顶栏读数条 + 拨档导航 + 行内展开仪表。最硬朗，最像一台仪器；列表会被展开项推长。」）

## 落地范围

- 保留 4 个页签与全部现有交互逻辑（AI gate、强制删除、还原点、逐项审核），只改呈现
- 顶栏读数条 + 左侧 196px 拨档列（含底部容量刻度）+ 行内展开仪表替代右侧常驻详情栏
- 色彩延续 graphite + amber + 红/绿/橙语义，但 `--dimmer` 那档 3.1:1 的灰必须换掉
- 字体只用系统栈（`"Segoe UI"` / `"Microsoft YaHei UI"` / `"Cascadia Mono"`）——
  `styles.css` 里声明的 Space Grotesk 从未真正加载（index.html 无字体引用），按实测可用字体设计
- 其他三个页签（卸载历史 / 应用权限 / 设置）沿用同一套 token 与分区语法，不重做信息架构

## 落地结果（已完成）

| 文件 | 改了什么 |
|---|---|
| `src/App.tsx` | 软件列表提升到 App 层（挂载枚举一次，卸载后统一刷新）；`grid 62px + 1fr` 外壳；顶栏读数条（已安装 / 受保护 / 已知占用）+ 两盏状态灯（管理员 / AI）；196px 拨档列改用真 `<button>`，底部放可清理-受保护双段占位条 |
| `src/views/AppsView.tsx` | 双栏改成单列平铺：工具条（查找 + `Ctrl K` + 来源筛选 + 计数 + 重新枚举）→ 34px 粘性表头 → 44px 行 → 行下展开仪表；点已展开行收起；末尾 `已列出全部 N 项` |
| `src/views/AppDetail.tsx` | 三个 `.section` 改成 `.insp` 三栏（安装事实 / 判定 / 动作），props 未变；长路径按 `\` 插 `<wbr>`，断行不再切断 `setup.exe` |
| `src/styles.css` | token 换到 `#0E1117 / #12161D / #262C38`，`--dim`→`#98A0AF`、`--dimmer`→`#7C8598`（4.9:1）；`--radius: 0`；字体改系统栈；新增顶栏 / 拨档 / 清单 / 行内仪表，删掉 `.list-row` 全套；窄窗口 ≤1180px 仪表折成两栏、动作区横跨底部 |
| `tools/preview/main.tsx` | 预览外壳与真实外壳同构（原来的 `.glyph/.nav/.ai-dot` 已删，不同步就是白屏） |
| `tools/preview/probe.mjs` | 新增：CDP 求值 + 截图一体，用来量字号与截交互态 |

截图：`tools/preview/shots/*.png`，README 用图已同步到 `docs/screenshots/`。

未跑：`design_lint.py check / ledger`（本机没有 Python 版 playwright，会直接报「体检跳过」）。
字号与对比度是手工逐条核的：正文 12.5–13px、标签一律 ≥11px、`--dimmer` 从 3.1:1 提到 4.9:1。
