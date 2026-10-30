# Apple Music 下载器集成交接

更新日期：2026-09-02

本文是给后续 MusicPlayer 开发会话使用的事实清单和实现约束。它描述的是当前 Windows 原生的旧版 AAC 下载器，不是 WSL/ALAC 下载器。

## 1. 本版本范围

- 只集成 Apple Music AAC 下载，固定 `SongCodec.AAC_LEGACY`。
- 不启动、不检测、不调用 WSL、ALAC wrapper、`amdecrypt` 或 lossless 脚本。
- 播放器本地播放不依赖 Python；只有搜索、歌单解析和下载时才启动 Python worker。
- 下载队列由 Rust 后端持久化，Python 只负责 Apple Music 会话和单个下载任务。
- 不使用 SQLite。队列使用原子 JSON 快照，运行日志使用轮转 JSONL，只保留近期记录。
- 音乐库根目录是 `D:\Music`；新下载按专辑归档，已有文件不得移动或覆盖。

## 2. 当前可工作的部署

| 项目 | 当前值 |
|---|---|
| 上游 | <https://github.com/CharlesPikachu/musicdl> |
| 本地下载器根目录 | `C:\Users\giaos\Documents\Codex\2026-09-01\https-github-com-charlespikachu-musicdl-https\work\musicdl` |
| Git 基准提交 | `ddcdc020cec79f45da67cc4a5a4fb380abab43fb` |
| 已安装 musicdl | `2.13.7` |
| Python | `C:\Users\giaos\Documents\Codex\2026-09-01\https-github-com-charlespikachu-musicdl-https\work\musicdl\.venv\Scripts\python.exe` |
| 人工操作入口 | `...\run_apple_music.ps1` |
| Python 入口 | `...\scripts\apple_music_entry.py` |
| 令牌配置入口 | `...\configure_apple_music.ps1` |
| 令牌文件 | `...\.config\apple_music_token.dpapi` |
| FFmpeg | `tools\ffmpeg\ffmpeg-9.0.1-essentials_build\bin`，版本 `9.0.1` |
| N_m3u8DL-RE | `tools\n_m3u8\N_m3u8DL-RE.exe`，版本 `0.6.0+df70f0b...` |
| mp4decrypt | `tools\bento4\win-x64\Bento4-SDK-1-6-0-641.x86_64-microsoft-win32\bin\mp4decrypt.exe` |

2026-09-02 的实际账户检查结果：storefront `cn`，订阅有效，进程退出码 `0`。检查命令：

```powershell
& 'C:\Users\giaos\Documents\Codex\2026-09-01\https-github-com-charlespikachu-musicdl-https\work\musicdl\run_apple_music.ps1' -Check -Quality aac
```

当前人工命令：

```powershell
$downloader = 'C:\Users\giaos\Documents\Codex\2026-09-01\https-github-com-charlespikachu-musicdl-https\work\musicdl\run_apple_music.ps1'
& $downloader -Keyword 'song or artist' -Quality aac
& $downloader -PlaylistUrl 'https://music.apple.com/...' -Quality aac
```

注意：第二条会在解析后直接下载整个歌单，不是只解析。播放器集成不要调用这两个交互式模式；应新增机器 worker。

## 3. 安全边界

- `media-user-token` 由 `configure_apple_music.ps1` 通过隐藏输入读取。
- 令牌使用当前 Windows 用户的 DPAPI 加密，磁盘上只保存 `apple_music_token.dpapi`。
- `run_apple_music.ps1` 临时解密令牌，通过子进程环境变量 `MUSICDL_APPLE_TOKEN` 传给 Python。
- Python 启动后立即 `os.environ.pop(...)`；PowerShell 在 `finally` 中清空 BSTR 并删除环境变量。
- 绝对禁止把令牌放进命令行参数、JSONL、队列文件、日志、崩溃报告或前端状态。
- 也禁止输出 cookies、播放 URL、PSSH、解密 key、`DownloadItem.raw_data` 或完整 `SongInfo.todict()`。
- 播放器只显示 `configured / invalid / active subscription / storefront` 等脱敏状态。

建议新增 `run_apple_music_worker.ps1`：沿用现有 DPAPI 解密方式，然后以继承 stdin/stdout 的方式启动 Python worker。Rust 只启动该 PowerShell 入口，不自行读取令牌文件。

## 4. 本地仓库不是干净的上游副本

不要重新 clone 上游后直接开始集成。当前可工作的私有歌单解析和安静进度依赖尚未提交的补丁。

已修改：

- `.gitignore`
- `musicdl/modules/sources/apple.py`
- `musicdl/modules/sources/base.py`
- `musicdl/modules/utils/appleutils.py`
- `musicdl/musicdl.py`

关键未跟踪文件：

- `configure_apple_music.ps1`
- `run_apple_music.ps1`
- `scripts/apple_music_entry.py`

另外存在 ALAC/WSL 相关未跟踪脚本；本项目不得集成它们。

补丁的必要行为：

1. `apple.py` 支持 `/library/playlist/...`，调用 library playlist API，并分页读取。
2. 同时接受 `songs` 和 `library-songs`；library item 映射到 catalog ID，缺少歌词时仍可继续。
3. `appleutils.py` 容忍歌词或封面缺失。
4. `base.py` 与 `musicdl.py` 在 `disable_print=True` 时不创建 Rich 进度条。
5. 当前 Python 入口使用单行 `\r` 覆写；非 TTY 最多约每 15 秒打印一次，避免 CMD 6000 行缓冲区被刷满。

在迁移或重构前，应先把这个 working tree 的补丁和新增文件制作成可恢复提交或完整补丁包；只保存 `git diff` 不会包含未跟踪入口脚本。

## 5. 已验证行为和限制

- AAC 实际文件为 M4A/AAC-LC，立体声、44.1 kHz，实测音频码率约 261–277 kbps；入口对用户显示为 256 kbps 模式。
- 私有歌单 `https://music.apple.com/library/playlist/p.QvDQ8bbi7oqZQ09` 已验证可解析。
- 该歌单当时有 76 个条目，去重和区域可用性过滤后得到 69 个唯一可下载条目；其中 2 个重复，5 个在 CN Web Playback 不可用。
- 已做过单曲真实下载和 M4A 校验；歌单解析测试没有自动下载完整歌单。
- 搜索默认每次最多 10 条结果，Apple 客户端下载并发固定为 1。
- 当前入口是面向人的 CLI，还没有稳定的 JSONL 机器协议。
- 当前 `parseplaylist()` 后会由入口直接下载；播放器需要拆成“解析/加入队列”和“逐项下载”两个动作。
- musicdl 当前会创建 `D:\Music\AppleMusicClient\<timestamp> ...` 目录，并写 `search_results.pkl` / `download_results.pkl`。播放器不应依赖这些 PKL 或时间戳目录。
- Apple 下载临时目录目前是相对当前进程目录的 `apple_id_<catalog-id>`。worker 必须把 cwd 设置为每个任务的 staging 目录，或修改 `_download()` 使临时目录显式位于 `song_info.work_dir`。

当前 CLI 的退出码约定：`0` 成功，`2` 令牌配置文件缺失，`3` 订阅未激活，`4` 歌单未解析出项目或没有任何成功下载；其他异常一般为 `1`。机器 worker 应定义更细的稳定错误码，不要依赖异常文本。

## 6. 推荐的进程结构

```text
Svelte UI
  -> Tauri command/event
  -> Rust DownloadManager（队列、重试、取消、持久化）
  -> run_apple_music_worker.ps1（只负责 DPAPI 凭证桥接）
  -> apple_music_worker.py（Apple 会话、解析、下载、JSONL）
  -> N_m3u8DL-RE / mp4decrypt / ffmpeg
  -> D:\Music\.downloads\<job-id>
  -> 校验、整理、原子移动
  -> D:\Music\<Album Artist>\<Album> [Year]\...
```

推荐一个长生命周期 worker 会话，而不是把 `SongInfo` 序列化给 Rust：

- `SongInfo` 内含 cookies、下载对象和敏感/不可序列化字段。
- worker 搜索或解析后只返回安全字段和会话内 `handle`。
- Rust 选择条目后把 `handle` 发回同一 worker。
- worker 重启后旧 handle 作废；Rust 用 catalog ID、歌单 URL或搜索条件重新解析。
- 第一版下载并发保持为 1，先保证取消、重试和文件归档正确。

如果采用“一次命令一个进程”，下载前必须按 catalog ID 重新解析，不能 pickle/反序列化 `SongInfo` 跨边界。

## 7. JSONL 机器协议

stdin 和 stdout 均为 UTF-8 JSONL：一行恰好一个 JSON 对象。stdout 只允许协议事件；诊断信息写 stderr。不要在机器模式启动当前 `SingleLineStatus`。

建议命令：

```json
{"id":"cmd-1","command":"check"}
{"id":"cmd-2","command":"search","query":"Stairway to Heaven"}
{"id":"cmd-3","command":"parse_playlist","url":"https://music.apple.com/library/playlist/..."}
{"id":"cmd-4","command":"download","job_id":"...","handle":"session-track-...","staging_dir":"D:\\Music\\.downloads\\..."}
{"id":"cmd-5","command":"shutdown"}
```

建议事件：

```json
{"event":"ready","protocol":1,"capabilities":["aac","search","library-playlist"]}
{"event":"account","request_id":"cmd-1","configured":true,"active":true,"storefront":"cn"}
{"event":"track","request_id":"cmd-2","handle":"session-track-1","catalog_id":"...","title":"...","artist":"...","album":"...","duration_ms":0,"artwork_url":"...","available":true}
{"event":"playlist_ready","request_id":"cmd-3","name":"...","total":76,"downloadable":69,"duplicates":2,"unavailable":5}
{"event":"stage","job_id":"...","value":"downloading"}
{"event":"progress","job_id":"...","bytes_done":0,"bytes_total":null}
{"event":"complete","job_id":"...","staged_path":"D:\\Music\\.downloads\\...\\track.m4a","catalog_id":"...","tags":{"title":"...","artist":"...","album":"...","album_artist":"...","year":"...","disc":1,"disc_total":1,"track":1,"track_total":10,"compilation":false}}
{"event":"error","job_id":"...","code":"APPLE_UNAVAILABLE","message":"Track is not available in storefront cn","retryable":false}
{"event":"finished","request_id":"cmd-3","ok":true}
```

协议要求：

- 每个请求有 `id`，每个事件带 `request_id` 或 `job_id`，避免靠行序推断。
- `progress` 在值实际变化时才发，且节流到最多 4 Hz；无变化时不重复输出。
- 阶段至少区分 `resolving`、`downloading`、`decrypting`、`remuxing`、`tagging`、`verifying`、`organizing`。
- “不可用”“认证失败”“网络失败”“工具缺失”“磁盘空间不足”“校验失败”“取消”使用稳定错误 code。
- 一首歌最终必须恰有一个 terminal event：`complete`、`error` 或 `cancelled`。
- 所有协议字段都需有最大长度；异常对象和原始 API 响应不得直接 JSON 化。

前端下载列表至少记录：队列顺序、封面、标题、艺人、专辑、质量 `AAC 256`、阶段、进度、速度、ETA、目标路径、错误、重试/取消动作。

## 8. 专辑归档规则

下载阶段：

```text
D:\Music\.downloads\<job-id>\
```

最终路径：

```text
# 单碟
D:\Music\<Album Artist>\<Album> [Year]\01 - Title.m4a

# 多碟
D:\Music\<Album Artist>\<Album> [Year]\Disc 1\01 - Title.m4a
```

规则：

- 路径元数据优先使用 `DownloadItem.media_tags`，不要只使用 `SongInfo.album/singers`。
- 可用字段包括 `album`、`album_artist`、`album_id`、`artist`、`artist_id`、`date`、`disc`、`disc_total`、`track`、`track_total`、`title`、`title_id`、`compilation`、`gapless`、`genre` 和 `storefront`。
- `title_id` 会写入 M4A 的 `cnID`，应作为 Apple 曲目去重主键；`album_id` 对应 `plID`。
- compilation 使用 `Various Artists`；缺少专辑使用 `Unknown Album`；缺少专辑艺人再回退到 track artist。
- Windows 路径需清理保留名、非法字符、尾随点/空格，并控制完整路径长度。
- 同名且同 catalog ID：识别为重复，不覆盖。
- 同名但不同 catalog ID：文件名追加 `[<catalog-id>]`。
- `.lrc` 与曲目一起移动；每个专辑只保存一份 `cover.jpg`，已存在时不覆盖。
- `D:\Music\.downloads` 必须被音乐库扫描器排除。
- 只整理本播放器新下载的文件，不批量重排用户已有音乐。

提交到音乐库前必须：

1. 下载器报告完成。
2. 用 `ffprobe` 验证容器、AAC 音轨、可读时长和非零文件大小。
3. 校验关键标签并生成最终路径。
4. 因 staging 与最终库同在 `D:`，使用同卷原子 move。
5. move 成功后才通知播放器库索引新增文件。

## 9. 队列、日志和取消

- Rust 是队列唯一事实来源；Python worker 不维护长期历史。
- 建议状态目录：`%LOCALAPPDATA%\MusicPlayer\state\download-queue.json`。
- 每次状态变更写临时文件、flush 后 replace，避免崩溃留下半个 JSON。
- 日志目录：`%LOCALAPPDATA%\MusicPlayer\logs`，按大小或日期轮转，例如最多 5 个文件、每个 2 MiB；不保存长期下载历史。
- 应用重启后，`queued` 可继续，`running` 转为 `interrupted` 并允许重试；已经通过 ffprobe 且已原子移动的项目按完成处理。
- 启动下载器及其子程序时使用 Windows Job Object（推荐）或等价的子进程树管理。取消必须终止 N_m3u8DL-RE/mp4decrypt/ffmpeg 整棵树，并等待句柄退出。
- 取消后只清理精确的 `D:\Music\.downloads\<job-id>`；不得对 `D:\Music` 做递归清理。
- worker 意外退出时，当前任务标记为可重试错误，其余排队项保留。

## 10. 下一会话的建议实施顺序

1. 初始化 `D:\Projects\MusicPlayer` 工程，并先写入本项目的路径/数据约束。
2. 为旧下载器制作可恢复补丁或提交；确认私有歌单补丁不会丢失。
3. 新增 `scripts\apple_music_worker.py` 和安全的 `run_apple_music_worker.ps1`，机器模式固定 AAC。
4. 先完成 `check`、`search`、`parse_playlist`，用协议测试保证 stdout 每行均为合法 JSON，且没有凭证或 raw API 数据。
5. 完成单曲 `download`、阶段事件、节流进度和取消；worker cwd 指向 per-job staging。
6. 用 ffprobe 验证，并实现基于 `media_tags` 的专辑归档器和不覆盖策略。
7. Rust 实现 JSON 快照队列、worker supervisor 和 Windows Job Object。
8. 最后接入 Svelte 搜索页、歌单预览、下载列表和音乐库增量刷新。

## 11. 最小验收集

- 未配置/失效令牌时 UI 给出可操作提示，日志中无令牌。
- storefront/订阅检查可显示脱敏状态。
- 搜索返回安全元数据，可选择加入队列，不立即下载所有结果。
- 上述私有 library playlist 可解析，重复和 unavailable 项有明确状态。
- 长时间运行不会不断追加进度行；机器 stdout 始终为合法 JSONL。
- 下载一首实际歌曲后，ffprobe 通过，文件位于正确专辑目录，M4A 标签完整。
- 同一曲目再次下载不会覆盖；不同 ID 同名不会冲突。
- 取消时整个子进程树退出，只有对应 job staging 被清理。
- 应用重启后队列恢复，不需要 SQLite，也不保留过久历史。
- 播放器完全关闭 Python/下载器后，本地播放、无缝切歌和音乐库浏览仍正常。

## 12. 明确不要做的事

- 不调用 `run_apple_music_lossless.ps1`、`start_apple_lossless_wrapper.ps1` 或任何 WSL service。
- 不把网页登录状态直接当作播放器认证接口；当前下载器使用已保存的 DPAPI token。
- 不解析当前人类 CLI 的彩色文本或进度条。
- 不通过前端传输令牌，不把完整 `SongInfo`/`DownloadItem` 暴露到 IPC。
- 不使用 SQLite，不无限追加日志，不保留完整长期下载历史。
- 不让下载器直接写最终音乐库后再猜测元数据；先 staging、校验、归档，再入库。
