---
id: design-system-deep-dive
title: Design System Deep Dive — Shipped-Product Token Craft
domain: design-systems
category: design-systems
difficulty: advanced
tags: [tokens, typography, color, components, motion, elevation, layout, accessibility, anti-ai-slop, DESIGN.md]
quality_score: 80
last_updated: 2026-06-22
---
# Design System Deep Dive — Shipped-Product Token Craft

> 这是给 UIUX 阶段的**进阶**方法论，把"哪些 token、哪个字阶、哪条缓动"从口号变成**照抄的具体值**。
> 取材自真实上线产品的设计系统（Stripe / Linear / Vercel / Claude / Sanity / Runway / Raycast 等），
> 只保留**可复用的工程动作**。与 `anti-ai-slop.md`（反面清单 + 自评门）互补：那篇讲"别做什么"，
> 本篇讲"**正向的体系怎么搭**"。
>
> 一句话原则：**先 commit 一个具名方向 + 1–3 个真实参照，再把每个 token 都问"为什么是它"。**
> 精品与 generic 的差距，全在 token 背后的"为什么"。

---

## 1. Token 架构：三层，组件只引语义层

真实系统都是**三层 token**，组件**永不**直接写原始值：

```
Primitive（原始）   --blue-600: #2563eb            原子调色板，不带语义
Semantic（语义）    --color-primary: var(--blue-600)   意图："这是主操作色"
Component（组件）   --button-bg: var(--color-primary)  用法："按钮背景用主色"
```

- **铁律**：组件只引用 `--color-*` / `--space-*` / `--text-*` 这类**语义/组件 token**，从不写裸 hex 或引用 primitive。
  改一个 primitive，所有引用它的语义 token 自动更新——这就是"theme once, propagate everywhere"。
- **深色模式 = 只覆盖语义层**：`@media (prefers-color-scheme: dark)` 里重定义 `--color-bg / --color-surface / --color-text / --color-border / --color-shadow`，**不动 primitive**。
- Token 不只有颜色。一套完整 token 至少覆盖 **8 个类别**：
  `color · typography(size/weight/line-height/tracking) · spacing · radius · border/hairline · elevation/shadow · z-index(语义命名) · motion(duration/easing)`。
  缺哪类，前端就会就地硬编码——那正是漂移的起点。

---

## 2. Color：一个主色面 + 一个手术刀 accent

- **60-30-10 分布**：60% 中性面 / 30% 次要面 / 10% 主色；**accent 占视口 ≤3%**，只给最高优先 CTA。
  "一个 band 里只有一个填充按钮"是 Stripe 的硬规矩——`--color-primary` 是 CTA + 链接强调色，**不是正文色**。
- **永不纯黑纯白**：正文用"近黑带品牌温度"（Stripe ink `#0d253d` 深海军蓝，而非 `#000`），底色用近白。
  纯 `#000/#fff` 是 generic tell。
- **中性色带温度**：中性灰向品牌色相偏 `+0.005~0.015` chroma（OKLCH 思维），别用纯灰 `#808080`。
- **语义角色**（最少 6 个，各带 default/hover/active）：
  `bg · surface · text · text-secondary · primary(+hover) · accent · border/hairline · error · success · warning · info`。
- **文本 token 按"强调度"命名，不按灰阶号**：`ink → body-strong → body → muted → muted-soft`（强调度）远胜 `gray-700/600/500`（机械灰号）——
  前者表达意图、改色不破层级。表面 token 则按"抬升级"命名：`canvas → surface-1 → surface-2 → surface-3`。
- **每个暗色面预配 `on-dark` 文本 token**（`on-dark / on-dark-soft / on-primary`）——把"暗面上用什么字色"提前解好，对比天然达标。
- **禁 AI 紫**：hue 250–310 作主色、`#6366f1 #7c3aed #8b5cf6 #a855f7 #764ba2 #667eea` 渐变——头号 AI 指纹。
- **禁"奶油米色带"**：OKLCH `L 0.84–0.97 · C<0.06 · hue 40–100`，以及 `--paper/--cream/--sand/--linen` 这种命名本身就是 tell。
- **对比度**：正文 ≥4.5:1、大字/UI ≥3:1。禁 gray-on-gray。

---

## 3. Typography：字阶大跳 + 战略字距 = 身份

排版是**最廉价也最强**的差异化杠杆。Stripe 用 weight 300 + 负字距做"编辑密度"，Linear 在 80px 上用 -3px（≈4% of size）。

- **字阶用比例，不随手取值**：data-dense 用 1.2，通用 1.25，营销/cinematic 可到 1.333+。
  建 `--text-xs … --text-3xl`（至少 7 级）；**标题与正文对比拉开**，display 可到 48–96px。
- **战略 letter-spacing（tracking）**——这是真实系统的签名动作：
  - **display 收紧**：`-0.04em ~ -0.01em`，字号越大收得越多（Stripe 56px → -1.4px ≈ -0.025em；Linear 80px → -3px ≈ -0.0375em）。
  - **eyebrow / ALL-CAPS 放开**：`+0.05em ~ +0.12em`——正字距把 eyebrow 标成"分类层"，与负字距 display 形成对撞。
  - **正文 0**。
- **字重**：一个页面 ≤3 个字重；标题与正文字重差 `≥300`（如 display 300 vs UI 400，或 body 400 vs heading 700）。
- **line-height**：标题 1.0–1.2（全大写下限 1.0），正文 1.4–1.7，长文 1.6+。
- **font-feature-settings 当签名（强差异化杠杆）**：在 `body` 上全局开一个 stylistic set（Stripe `ss01`、Raycast `ss03`——换掉单层 `g/a`），
  默认 Inter 一旦开了 stylistic set 就"不再是模板 Inter"。数字/金额单元格用 `tnum`（tabular-nums，对齐 + 暗示"数据/金融 DNA"）。
- **字体选择**：先写**三个具象气质词**（"warm and mechanical and opinionated"，不是"modern"），再据此选字。
  display + body 在**对比轴**上配对（高对比衬线 + 几何 sans，或 grotesk + mono）。
  **reflex-reject 默认禁用**（除非品牌 brief 点名）：`Inter / Roboto / Open Sans / Lato / Montserrat / Poppins / Nunito / Space Grotesk`。
  真实系统的开源替身：Stripe→Inter@300+ss01+负字距；Linear→Inter 500/600/700 或 Geist Sans；mono→JetBrains Mono / Geist Mono。

---

## 4. Elevation & Depth：阴影表达 z 轴，不是统一糊一层

真实系统**很少**用厚 drop-shadow。两种主流深度体系：

- **亮色面（Stripe）**：分级阴影表达 z 关系——贴地卡极轻 `0 1px 3px rgba(0,55,112,.08)`，浮层更深 `0 8px 24px`，按下变浅；同层一致。
  氛围/品牌 lift 交给"渐变 mesh / 背景图"而非字面阴影。
- **暗色面（Linear）**：**surface ladder + 1px hairline** 代替阴影——`surface-1 / surface-2 / surface-3` 逐级提亮，外加 `1px hairline`；
  暗底上几乎不投影。lifted 面板顶边加一道**极淡白色高光**，做出"像素渲染"质感。
- **焦点环是一级 elevation**：`2px primary outline @ 50% opacity`（或 2px solid + 2px offset）。
- **现代精品偏好**：`1px 内/外描边 + 极轻阴影` > 厚 drop-shadow（更干净、更工程感）。半透明/毛玻璃用于建层级，不是装饰。

---

## 5. Spacing / Layout / Radius：刻度化 + 节奏交替

- **间距刻度**：4px 或 8px 基（Linear 4px、Stripe 8px+2/4/12 微调）。常用 `4 8 12 16 24 32 48 64 96`。
  **never 随手取值**——每个 margin/padding 都是刻度步。
- **区块节奏**：section 间距 64–96px（`--space-section`）；区块内组间 24–48px；组内 8–24px。
- **容器**：内容栏 ~1200–1280px；长文阅读栏 ~640–720px。卡片栅格 `repeat(auto-fit, minmax(280px,1fr))`。
- **圆角刻度**：建 `--radius-xs…xl + pill(9999px)`（如 4/6/8/12/16/9999）。**全站按钮统一形状**（要么都 pill 要么都圆角矩形，别混）。
- **节奏交替（破对称）**：别堆 3 个等布局区块。交替"全宽↔约束 / 图左↔图右 / 亮底↔暗底"。
  **"对称读作'生成的'，非对称读作'有意的'"**——先选一个具名页面骨架再写代码，多页不要重复同一骨架。
- **z-index 用语义命名层级**（`--z-dropdown/--z-modal/--z-toast`），绝不 `999/9999`。

---

## 6. 组件：token 引用式定义 + 全状态

真实 DESIGN.md 的组件**全部用 token 名定义**（`background --color-primary, padding --space-sm --space-lg, rounded --radius-pill`），
绝不写裸值——这样组件天然随 token 变。

- **每个交互组件做满 7 态**：default / hover / focus(可见焦点环) / active(按下) / disabled / loading / error。
- **每个数据视图做满 5 态**：空 / 加载(骨架) / 错误 / 正常 / 极多——空态要有引导 CTA，不是空白。
- **建"签名组件"**：每个系统都有 1–2 个标志组件（Stripe 的 composited dashboard mockup + tabular-money type；Linear 的 surface-ladder 卡）。
  挑一个**专属于这个产品**的组件重点打磨，它就是记忆点。
- **容器嵌套 ≤2 层**（卡中卡中卡 = 失败）。

---

## 7. Motion：时长分桶 + 自然缓动 + 一次编排入场

- **时长分桶**：`fast 120ms / base 220ms / slow 420ms`（或 100/300/500）；**退场 ≈ 进场的 75%**；<80ms 视为"瞬时"。
- **缓动**：`--ease-out: cubic-bezier(0.16,1,0.3,1)`（quint-out）、quart `(0.25,1,0.5,1)`、expo。
  **禁 bounce `(0.34,1.56,0.64,1)` / elastic 过冲**（toy-like）。
- **一次编排好的入场**（staggered reveal，`calc(var(--i)*50ms)` 封顶 500ms）胜过满屏散乱微交互。
- **只动 transform/opacity**，禁 animate width/height（触发 layout，掉帧 + CLS）。
- **`@media (prefers-reduced-motion: reduce)` 块必写**——把 transition/animation 降到接近 0。
- 动画**必须自证存在**：引导注意 / 表达空间连续 / 给反馈。纯装饰动画删掉。

---

## 8. 信息架构：证据优先于装饰

- **优先级**：真实截图 / 信任模块（客户 logo、真实证言）/ 证据点 / 任务流 **>** 装饰性 hero。
  Stripe 的论点是"看真实产品"——每个 feature 配一张 composited product mockup。
- **真实内容**：真实文案/截图/数据。禁 Lorem ipsum、禁"Welcome to [App]"、禁编造指标（`10x faster / 99.9% / trusted by 50,000+` 无出处别写）。
- **禁占位**：`Jane Doe / John Smith / Acme / example.com`。
- **避免模板骨架**：`Hero→Features→Pricing→FAQ→CTA` 一条龙无变化 = slop；至少加 ≥1 个非常规 section（对比表 / 交互 demo / 真实数据可视化）。

---

## 9. 可访问性（设计阶段大胆，底线不破）

> 设计**生成时先大胆**，把无障碍强校验放到 review/quality；但对比、焦点、aria、触控这些**底线仍不可破**。

- 对比 ≥4.5:1（正文）/ ≥3:1（大字与 UI）；不只靠颜色传达状态。
- 焦点环可见且不可删；键盘可达，tab 顺序合理；modal trap focus、drawer 返回 focus。
- icon-only 按钮加 `aria-label`；用语义 landmark（nav/main/aside）；动态区用 live region。
- 触控目标 ≥44×44px（移动端），间距 ≥8px。

---

## 10. DESIGN.md 文档结构（推荐章节顺序）

真实系统的 DESIGN.md 用**固定章节顺序**，让 AI 能稳定复现：

```
## Overview          —— 一段话讲清"气质 + 一个记忆点 + 真实参照"
## Colors            —— Brand/Accent · Surface · Text · Semantic（每色带 token 名 + hex + 用途）
## Typography        —— Font Family · Hierarchy(表格:token|size|weight|line-height|tracking|use) · Principles · 开源替身说明
## Layout            —— Spacing(刻度) · Grid & Container · Whitespace 哲学
## Elevation & Depth —— 分级表(level|treatment|use) + 深度媒介说明
## Shapes            —— Radius 刻度表
## Components        —— 每个组件用 token 名定义 + 全状态；标注 Signature Components
## Do's and Don'ts   —— 各 5–8 条，"Don't" 直接对应这个产品的反面
## Responsive        —— 断点表 · 触控目标 · 折叠策略
## Self-critique      —— 6 维各打 1–5（Philosophy/Hierarchy/Execution/Specificity/Restraint/Variety），<3 必改
## Known Gaps         —— 诚实标注本文档"没定"的部分（如某些动效时长、校验态）
```

**让 AI 能稳定复现的关键技巧**：
- **任何刻度用表格**（字阶 / 圆角 / 间距 / elevation），列：`token | value | use`——给模型一个"封闭词表"。
- **每个值都是 token 引用**（`{color.primary}` / `var(--space-lg)`），组件里**永不**写裸值。
- **Do's and Don'ts 各 5–8 条**，把品味变成可勾选规则；"Don't" 要正对这个产品的反面（如"display 不超 600 字重"）。
- **`## Known Gaps` 诚实划界**：写清哪些没定，模型就不会瞎编值——比硬凑一个 generic 默认更好。
- **组件状态可有意省略 hover**：default/active(pressed)/focus/disabled 是契约，hover 交给实现——AI 文档里这是合理的收窄。

---

## 11. 生成顺序（每步定了再下一步）

1. **Design Read（一句话）**：什么页面 / 给谁 / 什么气质（3 个具象词）/ 选哪个家族 + 一行 AVOID。
2. **锁 token 表**：OKLCH/hex 调色板 → 语义 token；字体（display+body+可选 mono）+ 字阶 + 字距；图标库（单一）；间距/圆角/阴影/动效刻度。
3. **布局骨架**（可先 ASCII 线框）+ 动效规格 + 1 个签名组件。
4. **才写实现**，只引用已锁 token；组件做满 7 态、数据视图做满 5 态。

**终极判据（thumbnail test）**：把成品缩成缩略图，应一眼认出是"这个产品"，而非"又一个 AI 页面"。认不出 = 你交了模板，重做。
