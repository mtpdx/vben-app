# vben-app

基于vue-vben-admin 构建 tauri 桌面应用模板。

## 快速开始

### 前置要求

- [Rust](https://www.rust-lang.org/tools/install)
- [Node.js](https://nodejs.org/en/download)
- [pnpm](https://pnpm.io/installation)

### 安装依赖

```bash
npm i -g corepack

pnpm install
```

### 应用开发

```bash
pnpm tauri dev
```

### 应用打包

```bash
pnpm tauri build
```

## 项目结构

```
.
.
├── apps/
│   ├── backend-mock/     # 模拟后端服务
│   └── web-naive/        # Web 应用前端代码
├── internal/             # 内部工具和配置
├── packages/             # 核心包和组件库
├── src-tauri/            # Tauri 应用源代码
│   ├── src/              # Rust 源代码
│   ├── tauri.conf.json   # Tauri 配置文件
│   └── Cargo.toml        # Rust 依赖配置
├── docs/                 # vue-vben-admin文档
├── scripts/              # 构建和部署脚本
└── vben-app.md           # 项目文档

```

## 项目配置

### Tauri 配置文件

- `src-tauri/tauri.conf.json` # Tauri 配置文件
- `src-tauri/Cargo.toml` # Rust 依赖配置

### 应用配置

- `apps/web-naive/vite.config.ts` # Web 应用 Vite 配置文件
- `apps/web-naive/src/preferences.ts` # 可配置web应用主题、布局、功能开关等

## 开发指南

### 添加新页面

1. 在 apps/web-naive/src/views/ 目录下创建新页面组件
2. 在 apps/web-naive/src/router/routes/ 目录下添加路由配置
3. 如需要，更新菜单配置

### 自定义组件

自定义组件应放置在 packages/ 目录下相应的包中：

- UI 组件: packages/@core/ui-kit/
- 业务组件: packages/effects/common-ui/src/components/
- 工具组件: packages/utils/src/

### 国际化

国际化文件位于 apps/web-ele/src/locales/ 和 packages/locales/src/langs/ 目录下。支持中文和英文，默认语言可以在配置文件中设置。
