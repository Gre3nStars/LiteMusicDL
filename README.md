# LiteMusicDL

LiteMusicDL 是一个使用 React、TypeScript、Rust 与 Tauri 2 完全重写的桌面音乐播放器和下载管理器。运行时不依赖原 musicdl 项目的 Python 代码；音源以 Rust `MusicSource` 适配器接入。

## 已实现

- 多音源搜索与音源开关，默认启用 QQ 音乐和酷我音乐
- 网易云、咪咕音乐作为可选音源
- 播放、暂停、进度、音量、上一首、下一首和播放队列
- 收藏与最近播放（本地持久化）
- 下载目录原生选择、默认目录设置与下载任务管理
- 全屏歌词、逐行同步高亮
- 下载音频时自动保存同名 `.lrc`（音源提供歌词时）
- 播放与下载都由 Rust 适配层携带音源对应请求头执行；WebView 不再直接访问音源地址

## 音源状态

| 音源 | 默认 | 搜索 | 播放/下载 | 歌词 |
| --- | --- | --- | --- | --- |
| QQ 音乐 | 是 | 已接入 | 已接入 | 已接入 |
| 酷我音乐 | 是 | 已接入 | 已接入 | 已接入 |
| 网易云音乐 | 否 | 已接入 | 已接入 | 已接入 |
| 咪咕音乐 | 否 | 已接入 | 已接入 | 已接入 |

这些音源均来自原 musicdl 项目的适配范围。项目不会使用演示歌曲或伪造歌词填充界面。

## 开发

需要 Node.js、pnpm、Rust 和 Tauri 2 的系统依赖。

```bash
pnpm install
pnpm tauri dev
```

仅构建前端：

```bash
pnpm build
```

检查 Rust 后端：

```bash
cd src-tauri
cargo check
```

## 结构

- `src/App.tsx`：桌面界面、播放器状态、收藏、下载任务与歌词页
- `src/lib/bridge.ts`：TypeScript 到 Tauri 命令的边界
- `src-tauri/src/sources/`：纯 Rust 音源适配器
- `src-tauri/src/download.rs`：流式下载与安全文件名处理
- `src-tauri/src/lib.rs`：Tauri 命令、并行音源搜索和歌词写入

## 使用边界

仅用于个人学习和合法获取的内容。付费、订阅、地区限制或其他受保护内容需要用户通过对应平台取得授权；LiteMusicDL 不绕过 DRM、付费墙或访问控制。
